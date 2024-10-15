use actix::MailboxError;
use redis::RedisError;

use crate::JobBuilderError;

#[derive(Debug)]
pub enum Error {
    Redis(RedisError),
    CronError(cron::error::Error),
    MailboxError(MailboxError),
    BuidlerError(JobBuilderError),
    NoQueueRegister,
}

impl From<JobBuilderError> for Error {
    fn from(value: JobBuilderError) -> Self {
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
