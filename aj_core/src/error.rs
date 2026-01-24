use actix::MailboxError;
#[cfg(feature = "redis")]
use redis::RedisError;

#[derive(Debug)]
pub enum Error {
    #[cfg(feature = "redis")]
    Redis(RedisError),
    CronError(cron::error::Error),
    MailboxError(MailboxError),
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

impl From<MailboxError> for Error {
    fn from(value: MailboxError) -> Self {
        Self::MailboxError(value)
    }
}
