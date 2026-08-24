#[cfg(feature = "redis")]
use redis::RedisError;

#[derive(Debug)]
pub enum Error {
    #[cfg(feature = "redis")]
    Redis(RedisError),
    /// Postgres backend failure. Carries the formatted error rather than the concrete type,
    /// because the driver, the pool and pool construction each have their own error type and
    /// they all need to land in one variant.
    #[cfg(feature = "postgres")]
    Postgres(String),
    CronError(cron::error::Error),
    ActorError(String),
    NoQueueRegister,
    SerializeError,
}

#[cfg(feature = "redis")]
impl From<RedisError> for Error {
    fn from(value: RedisError) -> Self {
        Self::Redis(value)
    }
}

#[cfg(feature = "postgres")]
impl From<deadpool_postgres::tokio_postgres::Error> for Error {
    fn from(value: deadpool_postgres::tokio_postgres::Error) -> Self {
        Self::Postgres(format!("{value}"))
    }
}

#[cfg(feature = "postgres")]
impl From<deadpool_postgres::PoolError> for Error {
    fn from(value: deadpool_postgres::PoolError) -> Self {
        Self::Postgres(format!("{value}"))
    }
}

#[cfg(feature = "postgres")]
impl From<deadpool_postgres::CreatePoolError> for Error {
    fn from(value: deadpool_postgres::CreatePoolError) -> Self {
        Self::Postgres(format!("{value}"))
    }
}

impl From<cron::error::Error> for Error {
    fn from(value: cron::error::Error) -> Self {
        Self::CronError(value)
    }
}

impl<M, E: std::fmt::Debug> From<kameo::error::SendError<M, E>> for Error {
    fn from(value: kameo::error::SendError<M, E>) -> Self {
        Self::ActorError(format!("{:?}", value))
    }
}
