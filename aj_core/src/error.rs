#[cfg(feature = "redis")]
use redis::RedisError;

#[derive(Debug)]
pub enum Error {
    #[cfg(feature = "redis")]
    Redis(RedisError),
    /// Postgres backend failure. Carries the formatted error rather than the concrete
    /// type so that both `postgres::Error` and `r2d2::Error` can map into one variant.
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
impl From<postgres::Error> for Error {
    fn from(value: postgres::Error) -> Self {
        Self::Postgres(format!("{value}"))
    }
}

#[cfg(feature = "postgres")]
impl From<r2d2::Error> for Error {
    fn from(value: r2d2::Error) -> Self {
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
