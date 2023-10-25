use std::error::Error;
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::mem;
use std::mem::{ManuallyDrop, MaybeUninit};
use try_drop::adapters::{
    FallbackTryDropStrategyHandler, FallibleTryDropStrategyRef, TryDropStrategyRef,
};
use try_drop::{ImpureTryDrop as TryDrop, PureTryDrop, TryDropStrategy};

/// A rollback for a transaction.
///
/// This returns a guard struct with an operation that will be performed when this gets dropped.
///
/// To prevent this, mark the operation as successful. To do this call
/// [`RollbackGuard::ok`].
///
/// If the rollback operation can not fail (`E` is `()`) it also implements the normal `Drop`
/// trait and requires no special handling.
/// For convenience you can use [`infallible_rollback`] for this.
/// Note that `()` is used instead of `!` for the error type in this case, because `!` is not
/// stabilized at the time of writing. If the error type `E` is `()` the rollback should not
/// return the error variant, it will not be handled.
///
/// If rollback operation may fail (`E` is [`RollbackError`]), the guard implements [`Drop`] via
/// `try-drop`  instead. In this case you should install a [`TryDropStrategy`] to handle potential
/// rollback failures. Or alternatively you can call [`RollbackGuard::do_rollback`] manually and
/// handle it directly.
///
/// If you want to handle the potential success type `T` you will also need to manually do the
/// rollback via [`RollbackGuard::do_rollback`].
///
/// The registered rollback should not panic, but it can.
pub fn rollback<'a, F, T, E>(rollback_action: F) -> RollbackGuard<'a, T, E>
where
    F: FnOnce() -> Result<T, E> + 'a,
    E: MaybeError,
    RollbackGuard<'a, T, E>: private::DropLike,
{
    RollbackGuard {
        rollback_action: MaybeUninit::new(Box::new(rollback_action)),
        _error_type: PhantomData,
    }
}

/// A rollback that can not fail.
///
/// See [`rollback`] for more information.
///
/// The registered rollback function should not panic, but it can.
///
/// Calling [`RollbackGuard::do_rollback`] on the returned guard will return a `Result` which
/// is guaranteed to be `Ok`.
pub fn infallible_rollback<'a, F, T>(rollback_action: F) -> RollbackGuard<'a, T, ()>
where
    F: (FnOnce() -> T) + 'a,
    RollbackGuard<'a, T, ()>: private::DropLike,
{
    rollback(Box::new(|| Ok(rollback_action())))
}

/// Either a [`RollbackError`] with an inner [`Error`] or `()`.
pub trait MaybeError {}

impl MaybeError for () {}

impl<E: Error + Send + Sync + 'static> MaybeError for RollbackError<E> {}

/// An error during a rollback.
#[derive(Debug)]
pub struct RollbackError<E>(pub E)
where
    E: Send + Sync + 'static;

impl<E> Display for RollbackError<E>
where
    E: Display + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rollback error: {}", self.0)
    }
}

impl<E> Error for RollbackError<E> where E: Error + Send + Sync + 'static {}

/// Trait for a type that can be rolled back.
pub trait Rollback {
    type RollbackOk;
    type RollbackError;

    /// Performs the rollback.
    fn do_rollback(self) -> Result<Self::RollbackOk, Self::RollbackError>;
}

/// A rollback for a transaction.
///
/// To create this and for more information see [`rollback`] and the
/// top-level module documentation.
pub struct RollbackGuard<'a, T, E>
where
    Self: private::DropLike + 'a,
{
    rollback_action: MaybeUninit<Box<dyn FnOnce() -> Result<T, E> + 'a>>,
    _error_type: PhantomData<E>,
}

impl<'a, T, E> Rollback for RollbackGuard<'a, T, E>
where
    E: MaybeError,
    Self: private::DropLike,
{
    type RollbackOk = T;
    type RollbackError = E;

    /// Performs the rollback, consuming the guard.
    fn do_rollback(self) -> Result<T, E> {
        let mut slf = ManuallyDrop::new(self);
        // SAFETY: Since we do not drop `Self` (because of the `ManuallyDrop`) its `Drop` code
        // will not run, and thus the call below will be the only call to `_do_rollback`.
        unsafe { slf._do_rollback() }
    }
}

impl<'a, T, E> RollbackGuard<'a, T, E>
where
    E: MaybeError,
    Self: private::DropLike,
{
    /// Drops the rollback guard but does not run the rollback function.
    pub fn ok(self) {
        // Forgetting `self` will prevent the rollback from happening.
        mem::forget(self);
    }

    /// Makes the rollback mandatory, by returning a type that wraps this guard, implements
    /// [`Rollback`] as well but does not provide [`Self::ok`]. Note that the returned
    /// wrapped guard can still be prevented from executing on [`Drop`] by using
    /// functionality like [`mem::forget`].
    pub fn mandatory(self) -> MandatoryRollbackGuard<'a, T, E> {
        MandatoryRollbackGuard(self)
    }

    /// Does the rollback.
    ///
    /// # Safety
    /// The caller must ensure this is called at most once during the lifetime of the guard.
    unsafe fn _do_rollback(&mut self) -> Result<T, E> {
        // We use mem::replace because `rollback_action` is an `FnOnce` and we can only call it once.
        // SAFETY: The caller guarantees `_do_rollback` is not called again.
        // CLIPPY: This is OK because we never interact with `rollback_action` ever again;
        //         we don't plan to put something there again.
        #[allow(clippy::mem_replace_with_uninit)]
        let action = mem::replace(&mut self.rollback_action, mem::zeroed());
        // SAFETY: `Self::rollback_action` is guaranteed to be init. the first time this function
        // is called and the caller guarantees `_do_rollback` is not called again; see above.
        (action.assume_init())()
    }
}

impl<'a, T, E> TryDrop for RollbackGuard<'a, T, RollbackError<E>>
where
    E: Error + Send + Sync + 'static,
{
    type Error = RollbackError<E>;

    unsafe fn try_drop(&mut self) -> Result<(), Self::Error> {
        // SAFETY: we called this function inside a `TryDrop::try_drop` context.
        unsafe { self._do_rollback() }.map(|_| ())
    }
}

/// Drop code in case the rollback can fail.
///
/// The drop code is taken from [`try_drop::adapters::DropAdapter`].
impl<'a, T, E> private::DropLike for RollbackGuard<'a, T, RollbackError<E>>
where
    E: Error + Send + Sync + 'static,
    Self: TryDrop,
{
    unsafe fn drop(&mut self) {
        // SAFETY: we called this function inside a `Drop::drop` context.
        let result = unsafe { TryDrop::try_drop(self) };
        if let Err(error) = result {
            let handler = FallbackTryDropStrategyHandler::new(
                TryDropStrategyRef(self.fallback_try_drop_strategy()),
                FallibleTryDropStrategyRef(self.try_drop_strategy()),
            );

            handler.handle_error(error.into())
        }
    }
}

/// Drop code in case the rollback can not fail.
impl<'a, T> private::DropLike for RollbackGuard<'a, T, ()> {
    unsafe fn drop(&mut self) {
        // SAFETY: we called this function inside a `Drop::drop` context.
        unsafe { self._do_rollback() }.ok();
    }
}

// The actual `Drop` implementation, which just uses the private `DropLike` trait to do the drop.
// This is implemented this way, because `Drop` can not be specialized.
impl<'a, T, E> Drop for RollbackGuard<'a, T, E>
where
    Self: private::DropLike,
{
    fn drop(&mut self) {
        // SAFETY: we called this function inside a `Drop::drop` context.
        unsafe { private::DropLike::drop(self) }
    }
}

/// A rollback that is guaranteed to run on [`Drop`].
///
/// To create use [`RollbackGuard::mandatory`].
pub struct MandatoryRollbackGuard<'a, T, E>(RollbackGuard<'a, T, E>)
where
    RollbackGuard<'a, T, E>: private::DropLike;

impl<'a, T, E> Rollback for MandatoryRollbackGuard<'a, T, E>
where
    E: MaybeError,
    RollbackGuard<'a, T, E>: private::DropLike,
{
    type RollbackOk = T;
    type RollbackError = E;

    /// Performs the rollback, consuming the guard.
    fn do_rollback(self) -> Result<T, E> {
        self.0.do_rollback()
    }
}

/// The sealed pattern prevents other traits from implementing any trait that is `Sealed`.
mod private {
    use super::RollbackError;
    use std::error::Error;

    pub trait Sealed {}
    /// This is basically [`Drop`]
    /// We need this trait since [`Drop`] can not be specialized.
    pub trait DropLike {
        /// # Safety
        /// The caller must guarantee this does not get called more than once.
        unsafe fn drop(&mut self) {}
    }

    impl Sealed for () {}
    impl<E: Error + Send + Sync + 'static> Sealed for RollbackError<E> {}
}
