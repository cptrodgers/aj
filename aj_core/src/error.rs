#[cfg(feature = "redis")]
use redis::RedisError;

#[derive(Debug)]
pub enum Error {
    #[cfg(feature = "redis")]
    Redis(RedisError),
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
