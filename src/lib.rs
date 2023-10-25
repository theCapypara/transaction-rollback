//!
//! This crates provides mechanisms to make it easier to implement operations
//! which need to be rolled back in the case of failure. In general it provides guard objects
//! and traits for wrapping set-up and tear-down routines for operations.
//!
//! There are two main mechanisms provided.
//!
//! # `rollback` guard
//! Using the [`infallible_rollback`] function you can generate a guard object, that when dropped
//! will run the closure you put into it:
//!
//! ```rust
//! # use std::cell::RefCell;
//! use transaction_rollback::infallible_rollback;
//!
//! let mut value = RefCell::new(false);
//! let rollback_guard = infallible_rollback(|| *value.borrow_mut() = true);
//! assert_eq!(false, *value.borrow());
//! drop(rollback_guard);
//! assert_eq!(true, *value.borrow());
//! ```
//!
//! Your rollback logic can also return a value. This value is not retrievable when the guard
//! is dropped, but you can manually invoke a rollback via [`Rollback::do_rollback`],
//! which will return the value:
//!
//! ```rust
//! use transaction_rollback::{infallible_rollback, Rollback};
//!
//! let rollback_guard = infallible_rollback(|| "I did a rollback!");
//! assert_eq!(Ok("I did a rollback!"), rollback_guard.do_rollback())
//! ```
//!
//! Note that this returns a `Result` type. This is because rollbacks can potentially fail.
//! However rollback guards create via [`infallible_rollback`] can not fail and will
//! never return an `Err`.
//!
//! To create a more general rollback guard that can potentially fail, use [`rollback()`].
//! Note that, since [`Drop`] runs the rollback, dropping the guard could fail. Because
//! of this the returned guard implements [`try_drop::TryDrop`]. You can register handlers
//! to handle the failure on drop.
//!
//! ```should_panic
//! # use std::borrow::Cow;
//! # use std::cell::RefCell;
//! # use std::rc::Rc;
//! # use try_drop::drop_strategies::PanicDropStrategy;
//! use transaction_rollback::{rollback, Rollback, RollbackError};
//!
//! // due to not having a real example to show here, we use this dummy example error type.
//! #[derive(Debug, thiserror::Error)]
//! #[error("{0}")]
//! struct ExampleError(&'static str);
//!
//! let state: RefCell<Option<bool>> = RefCell::new(None);
//!
//! let rollback_guard = rollback(|| match state.borrow_mut().as_mut() {
//!     None => Err(RollbackError(ExampleError("Can not rollback"))),
//!     Some(v) => {
//!         *v = !*v;
//!         Ok(())
//!     }
//! });
//!
//! // This instructs try_drop to panic on failed `TryDrop`s.
//! let drop_strategy = PanicDropStrategy { message: Cow::Borrowed("oh no") };
//! try_drop::install_global_handlers(drop_strategy.clone(), drop_strategy);
//!
//! // rollback_guard will now be dropped, which will panic, because the rollback fails, since
//! // state is None and we installed the PanicDropStrategy:
//! // `oh no: Rollback error: Can not rollback`
//! ```
//!
//! If a value is put into the state, the above example would not panic:
//!
//! ```rust
//! # use std::borrow::Cow;
//! # use std::cell::RefCell;
//! # use std::rc::Rc;
//! # use try_drop::drop_strategies::PanicDropStrategy;
//! # use thiserror::Error;
//! # use transaction_rollback::{rollback, Rollback, RollbackError};
//! # #[derive(Debug, Error)]
//! # #[error("{0}")]
//! # struct ExampleError(&'static str);
//! # let state: RefCell<Option<bool>> = RefCell::new(None);
//! # let state_clone = state.clone();
//! # let rollback_guard = rollback(|| match state.borrow_mut().as_mut() {
//! #     None => Err(RollbackError(ExampleError("Can not rollback"))),
//! #     Some(v) => {
//! #         *v = !*v;
//! #         Ok(())
//! #     }
//! # });
//! # let drop_strategy = PanicDropStrategy { message: Cow::Borrowed("oh no") };
//! # try_drop::install_global_handlers(drop_strategy.clone(), drop_strategy);
//! #
//! *state.borrow_mut() = Some(true);
//! drop(rollback_guard);
//! assert_eq!(Some(false), *state.borrow());
//! ```
//!
//! Alternatively, you can do the rollback with  [`Rollback::do_rollback`] which will directly
//! return the `Result` of the rollback.
//!
//! To prevent a [`RollbackGuard`] created via [`rollback()`] or [`infallible_rollback`] to
//! run the rollback logic, destroy the guard by running [`RollbackGuard::ok`] on it.
//!
//! ```rust
//! # use transaction_rollback::infallible_rollback;
//! let rollback_guard = infallible_rollback(|| unreachable!());
//! rollback_guard.ok()
//! // The rollback code will not run.
//! ```
//!
//! If you want a rollback to always occur, you can call [`RollbackGuard::mandatory`], which
//! returns a wrapper object which is still rolled back when dropped and still implements
//! [`Rollback`], but has no `ok` method.
//!
//! ```rust
//! # use transaction_rollback::infallible_rollback;
//! let rollback_guard = infallible_rollback(|| println!("I will run!")).mandatory();
//! // The rollback code will run.
//! ```
//!
//! # `Transaction` trait
//! The [`Transaction`] trait allows for operations to be implemented that can have a wide selection
//! of set-up and tear-down logic:
//!
//! - [`Transaction::execute`] is used by the caller to run the transaction.
//! - [`Transaction::operation`] is implemented to contain the main operation of the transaction.
//! - [`Transaction::before`] is implemented to run code before the actual operation.
//!   If it fails, the operation is not run, not rolled back and no [`Transaction::finally`]
//!   is called.
//! - [`Transaction::rollback`] is implemented to run the rollback logic, in case
//!   [`Transaction::operation`] fails. It can also fail.
//! - [`Transaction::finally`] is implemented to run after the operation and potential rollback. It
//!   can also fail.
//!
//! ```rust
//! use transaction_rollback::{Transaction, TransactionState};
//! struct MyImportantOperation;
//!
//! impl Transaction for MyImportantOperation {
//!     type BeforeError = ();
//!     type Ok = &'static str;
//!     type Error = ();
//!     type RollbackOk = ();
//!     type RollbackError = ();
//!     type FinallyError = ();
//!
//!     fn before(&mut self) -> Result<(), Self::BeforeError> {
//!         // You could so something meaningful here.
//!         Ok(())
//!     }
//!
//!     fn operation(&mut self) -> Result<Self::Ok, Self::Error> {
//!         Ok("very important!")
//!     }
//!
//!     fn rollback(&mut self, err_operation: &Self::Error) -> Result<Self::RollbackOk, Self::RollbackError> {
//!         // Rollback code in case of failure.
//!         Ok(())
//!     }
//!
//!     fn finally(&mut self, state: &TransactionState<Self::BeforeError, Self::Ok, Self::Error, Self::RollbackOk, Self::RollbackError, Self::FinallyError>) -> Result<(), Self::FinallyError> {
//!         // Finalization code that is always run.
//!         Ok(())
//!     }
//! }
//!
//! assert_eq!(TransactionState::Ok("very important!"), MyImportantOperation.execute())
//! ```
//!
//! If the type implementing [`Transaction`] is [`std::panic::UnwindSafe`], all of it's associated
//! types are also [`std::panic::UnwindSafe`] and [`std::panic::RefUnwindSafe`] and all of its
//! error types implement [`From`] for [`PanicError`], then the transaction will also implement
//! [`UnwindCheckedTransaction`], which provides [`UnwindCheckedTransaction::execute_unwind_checked`],
//! which is identical to [`Transaction::execute`] except it also catches all panics/unwinds and
//! converts them into the error types.

mod rollback;
mod transaction;

pub use try_drop;

pub use rollback::*;
pub use transaction::*;
