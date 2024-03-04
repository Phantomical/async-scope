use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::task::FutureObj;

use crate::error::Payload;
use crate::executor::Executor;
use crate::util::complete::RequirePending;
use crate::util::split_arc::{Full, Partial};
use crate::util::OneshotCell;
use crate::wrapper::{TaskAbortHandle, WrapFuture};
use crate::{scope, JoinError};

used_in_docs!(scope);

type ScopeExecutor<'env> = Executor<FutureObj<'env, ()>>;
