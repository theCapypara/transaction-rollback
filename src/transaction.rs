use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe, RefUnwindSafe, UnwindSafe};

/// State of a transaction
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionState<BE, O, E, RO, RE, FE> {
    /// The operation to prepare the transaction failed.
    FailedBefore(BE),
    /// The transaction completed successfully.
    Ok(O),
    /// The transaction needed to be rolled back.
    ///
    /// The first item is the original error of the operation.
    /// The second is the result of the rollback.
    Rollback(E, Result<RO, RE>),
    /// The transaction completed successfully, but `finally` failed.
    ///
    /// The first item is the success of the operation, the second the error of `finally`.
    OkButFailedFinally(O, FE),
    /// The transaction needed to be rolled back. Additionally `finally` failed.
    ///
    /// The first item is the original error of the operation.
    /// The second is the result of the rollback.
    /// The third is the error of `finally`.
    RollbackButFailedFinally(E, Result<RO, RE>, FE),
}

/// A trait for an operation that can be rolled back and/or that requires to be in a certain
/// state before/after running.
///
/// The [`Self::execute`] method is used to execute the transaction, see it's documentation
/// for more information.
pub trait Transaction: Sized {
    type BeforeError;
    type Ok;
    type Error;
    type RollbackOk;
    type RollbackError;
    type FinallyError;

    /// Execute the transaction. This will:
    /// - First call [`Self::before`], if it fails, it's error is returned
    ///  ([`TransactionState::FailedBefore`]).
    /// - Otherwise it will then call [`Self::operation`], if it succeeds it will continue to
    ///   `finally` with it's `Ok` value ([`TransactionState::Ok`]).
    /// - Otherwise it will try to rollback by calling [`Self::rollback`]
    /// ([`TransactionState::Rollback`]).
    /// - Afterwards [`Self::finally`] will be run. If it fails either
    /// [`TransactionState::OkButFailedFinally`] or [`TransactionState::RollbackButFailedFinally`]
    /// are returned, otherwise the state is unchanged. `finally` is not run if `before` failed.
    ///
    /// Panics are not caught, for this use [`UnwindCheckedTransaction`].
    #[allow(clippy::type_complexity)]
    fn execute(
        mut self,
    ) -> TransactionState<
        Self::BeforeError,
        Self::Ok,
        Self::Error,
        Self::RollbackOk,
        Self::RollbackError,
        Self::FinallyError,
    > {
        if let Err(e) = self.before() {
            TransactionState::FailedBefore(e)
        } else {
            let state = match self.operation() {
                Ok(o) => TransactionState::Ok(o),
                Err(e) => {
                    let rollback_result = self.rollback(&e);
                    TransactionState::Rollback(e, rollback_result)
                }
            };
            if let Err(e) = self.finally(&state) {
                match state {
                    TransactionState::Ok(oo) => TransactionState::OkButFailedFinally(oo, e),
                    TransactionState::Rollback(oe, rs) => {
                        TransactionState::RollbackButFailedFinally(oe, rs, e)
                    }
                    _ => unreachable!(),
                }
            } else {
                state
            }
        }
    }

    /// Performs operations to prepare the transaction. If this fails, no rollback is run.
    /// If it succeeds, the transaction can continue.
    fn before(&mut self) -> Result<(), Self::BeforeError>;

    /// Performs operations to prepare the transaction. If this fails, no rollback is run.
    /// If it succeeds, the transaction can continue.
    fn operation(&mut self) -> Result<Self::Ok, Self::Error>;

    /// Performs a rollback if the operation failed.
    fn rollback(
        &mut self,
        err_operation: &Self::Error,
    ) -> Result<Self::RollbackOk, Self::RollbackError>;

    /// Performs an action to always perform at the end of the operation, no matter if it had
    /// to be rolled back or not.
    ///
    /// The passed in `state` can be expected to be either `TransactionState::Ok` or
    /// `TransactionState::Rollback`.
    #[allow(clippy::type_complexity)]
    fn finally(
        &mut self,
        state: &TransactionState<
            Self::BeforeError,
            Self::Ok,
            Self::Error,
            Self::RollbackOk,
            Self::RollbackError,
            Self::FinallyError,
        >,
    ) -> Result<(), Self::FinallyError>;
}

/// A struct representing the value of a caught panic/unwind.
pub struct PanicError(pub Box<dyn Any + Send>);

/// Sub-trait of [`Transaction`] that is implemented for all [`UnwindSafe`] transactions that
/// have a [`From<PanicError>`] implementation for all it's error types.
///
/// It provides a method [`Self::execute_unwind_checked`] that executes the transaction while
/// catching all unwinds.
///
/// Implementors must make sure that no safety invariants are violated by panics inside
/// any of the transaction methods (that is all operations are [`UnwindSafe`]).
pub trait UnwindCheckedTransaction: Transaction + UnwindSafe
where
    <Self as Transaction>::BeforeError: From<PanicError> + UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::Ok: UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::Error: From<PanicError> + UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::RollbackOk: UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::RollbackError: From<PanicError> + UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::FinallyError: From<PanicError> + UnwindSafe + RefUnwindSafe,
{
    /// See [`Transaction::execute`].
    ///
    /// Additionally an unwind (`panic`) in any of the steps of the transaction is caught and
    /// turned into the corresponding error type.
    #[allow(clippy::type_complexity)]
    fn execute_unwind_checked(
        mut self,
    ) -> TransactionState<
        Self::BeforeError,
        Self::Ok,
        Self::Error,
        Self::RollbackOk,
        Self::RollbackError,
        Self::FinallyError,
    > {
        if let Err(e) = _catch_unwind(|| self.before()) {
            TransactionState::FailedBefore(e)
        } else {
            let state = match _catch_unwind(|| self.operation()) {
                Ok(o) => TransactionState::Ok(o),
                Err(e) => {
                    let rollback_result = _catch_unwind(|| self.rollback(&e));
                    TransactionState::Rollback(e, rollback_result)
                }
            };
            if let Err(e) = _catch_unwind(|| self.finally(&state)) {
                match state {
                    TransactionState::Ok(oo) => TransactionState::OkButFailedFinally(oo, e),
                    TransactionState::Rollback(oe, rs) => {
                        TransactionState::RollbackButFailedFinally(oe, rs, e)
                    }
                    _ => unreachable!(),
                }
            } else {
                state
            }
        }
    }
}

fn _catch_unwind<F, T, E>(op: F) -> Result<T, E>
where
    F: (FnMut() -> Result<T, E>),
    E: From<PanicError>,
{
    // We can assert it is UnwindSafe even though the operations may get a mutable Self,
    // because of the requirement of the trait [`UnwindCheckedTransaction`].
    match catch_unwind(AssertUnwindSafe(op)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(PanicError(e).into()),
    }
}

impl<T> UnwindCheckedTransaction for T
where
    T: Transaction + UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::BeforeError: From<PanicError> + UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::Ok: UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::Error: From<PanicError> + UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::RollbackOk: UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::RollbackError: From<PanicError> + UnwindSafe + RefUnwindSafe,
    <Self as Transaction>::FinallyError: From<PanicError> + UnwindSafe + RefUnwindSafe,
{
}
