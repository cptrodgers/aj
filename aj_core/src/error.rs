use actix::MailboxError;
use redis::RedisError;

#[derive(Debug)]
pub enum Error {
    Redis(RedisError),
    ExecutionError(String),
    CronError(cron::error::Error),
    MailboxError(MailboxError),
    BuidlerError(derive_builder::UninitializedFieldError),
    NoQueueRegister,
}

impl From<derive_builder::UninitializedFieldError> for Error {
    fn from(value: derive_builder::UninitializedFieldError) -> Self {
        Self::BuidlerError(value)
    }
}

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
