use redis::{Client, Commands, Direction, IntoConnectionInfo, RedisResult};

use crate::types::{Backend, QueueDirection};
use crate::Error;

#[derive(Debug, Clone)]
pub struct Redis {
    client: Client,
}

impl Redis {
    /// Redis::new("redis://localhost:6379/");
    pub fn new<T: IntoConnectionInfo>(connection_params: T) -> Self {
        let client = Client::open(connection_params).unwrap();
        Self { client }
    }

    pub fn new_with_client(client: Client) -> Self {
        Self { client }
    }

    pub fn lpush(&self, queue_name: &str, item: &str) -> RedisResult<()> {
        let mut conn = self.client.get_connection()?;
        let _: () = conn.lpush(queue_name, item)?;
        Ok(())
    }

    pub fn lmove(
        &self,
        from_queue: &str,
        to_queue: &str,
        count: usize,
        from_direction: Direction,
        to_direction: Direction,
    ) -> RedisResult<Vec<String>> {
        let mut conn = self.client.get_connection()?;
        let mut items = vec![];
        for _ in 0..count {
            let result: Option<String> = conn.lmove(
                from_queue,
                to_queue,
                clone_direction(&from_direction),
                clone_direction(&to_direction),
            )?;

            if let Some(item) = result {
                items.push(item)
            }
        }

        Ok(items)
    }

    pub fn lrange(
        &self,
        queue: &str,
        count: usize,
        queue_direction: QueueDirection,
    ) -> RedisResult<Vec<String>> {
        let mut conn = self.client.get_connection()?;
        let items = match queue_direction {
            QueueDirection::Front => conn.lrange(queue, 0, count as isize - 1)?,
            QueueDirection::Back => {
                let mut res: Vec<String> = conn.lrange(queue, -(count as isize), -1)?;
                res.reverse();
                res
            }
        };
        Ok(items)
    }

    pub fn llen(&self, queue: &str) -> RedisResult<usize> {
        let mut conn = self.client.get_connection()?;
        conn.llen(queue)
    }

    pub fn lrem(&self, queue: &str, item: &str) -> RedisResult<usize> {
        let mut conn = self.client.get_connection()?;
        conn.lrem(queue, 0, item)
    }
}

impl Backend for Redis {
    fn queue_push(&self, queue_name: &str, item: &str) -> Result<(), Error> {
        self.lpush(queue_name, item)?;
        Ok(())
    }

    fn queue_move(
        &self,
        from_queue: &str,
        to_queue: &str,
        count: usize,
        from_position: QueueDirection,
        to_position: QueueDirection,
    ) -> Result<Vec<String>, Error> {
        let from_direction = from_position.into();
        let to_direction = to_position.into();
        let res = self.lmove(from_queue, to_queue, count, from_direction, to_direction)?;
        Ok(res)
    }

    fn queue_remove(&self, queue: &str, item: &str) -> Result<(), Error> {
        self.lrem(queue, item)?;
        Ok(())
    }

    fn queue_get(
        &self,
        queue: &str,
        count: usize,
        direction: QueueDirection,
    ) -> Result<Vec<String>, Error> {
        let res = self.lrange(queue, count, direction)?;
        Ok(res)
    }

    fn queue_count(&self, queue: &str) -> Result<usize, Error> {
        let res = self.llen(queue)?;
        Ok(res)
    }

    fn storage_upsert(&self, hash: &str, key: &str, value: String) -> Result<(), Error> {
        let mut conn = self.client.get_connection()?;
        let _: () = conn.hset(hash, key, value)?;
        Ok(())
    }

    fn storage_get(&self, hash: &str, key: &str) -> Result<Option<String>, Error> {
        let mut conn = self.client.get_connection()?;
        let res: Option<String> = conn.hget(hash, key)?;
        Ok(res)
    }
}

impl From<QueueDirection> for Direction {
    fn from(value: QueueDirection) -> Self {
        match value {
            QueueDirection::Front => Direction::Left,
            QueueDirection::Back => Direction::Right,
        }
    }
}

fn clone_direction(direction: &Direction) -> Direction {
    match direction {
        Direction::Left => Direction::Left,
        Direction::Right => Direction::Right,
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::types::QueueDirection;

    fn init_redis() -> Redis {
        let client = Client::open("redis://localhost:6379/").unwrap();
        let backend = Redis::new_with_client(client.clone());
        backend
    }

    fn clean(channel: &str) {
        let client = Client::open("redis://localhost:6379/").unwrap();
        client
            .get_connection()
            .unwrap()
            .del::<&str, i32>(channel)
            .unwrap();
    }

    #[test]
    fn test_queue_push() {
        let queue_name = Uuid::new_v4().to_string();
        let backend = init_redis();

        // Push an item to the queue
        let result = backend.queue_push(&queue_name, "item1");
        assert!(result.is_ok());

        // Check that the queue contains the pushed item
        let items = backend
            .queue_get(&queue_name, 1, QueueDirection::Front)
            .unwrap();
        assert_eq!(items, vec!["item1".to_string()]);

        // Push another item and check
        backend.queue_push(&queue_name, "item2").unwrap();
        let items = backend
            .queue_get(&queue_name, 2, QueueDirection::Front)
            .unwrap();
        assert_eq!(items, vec!["item2".to_string(), "item1".to_string()]); // Inserting to front
        clean(&queue_name);
    }

    #[test]
    fn test_queue_move() {
        let backend = init_redis();

        let from_queue = "from_queue";
        let to_queue = "to_queue";

        // Push items to the from_queue
        backend.queue_push(from_queue, "item1").unwrap();
        backend.queue_push(from_queue, "item2").unwrap();

        // Move one item from the front of from_queue to the back of to_queue
        let result = backend.queue_move(
            from_queue,
            to_queue,
            1,
            QueueDirection::Front,
            QueueDirection::Back,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), vec!["item2".to_string()]); // item2 is at the front

        // Check that to_queue now contains the moved item
        let items = backend
            .queue_get(to_queue, 1, QueueDirection::Front)
            .unwrap();
        assert_eq!(items, vec!["item2".to_string()]);

        clean(&from_queue);
        clean(&to_queue);
    }

    #[test]
    fn test_queue_remove() {
        let queue_name = Uuid::new_v4().to_string();
        let backend = init_redis();

        // Push items to the queue
        backend.queue_push(&queue_name, "item1").unwrap();
        backend.queue_push(&queue_name, "item2").unwrap();

        // Remove an item from the queue
        let result = backend.queue_remove(&queue_name, "item1");
        assert!(result.is_ok());

        // Ensure the item was removed
        let items = backend
            .queue_get(&queue_name, 10, QueueDirection::Front)
            .unwrap();
        assert_eq!(items, vec!["item2".to_string()]);

        clean(&queue_name);
    }

    #[test]
    fn test_queue_get() {
        let queue_name = Uuid::new_v4().to_string();
        let backend = init_redis();

        // Push multiple items to the queue
        backend.queue_push(&queue_name, "item1").unwrap();
        backend.queue_push(&queue_name, "item2").unwrap();
        backend.queue_push(&queue_name, "item3").unwrap();

        // Retrieve items from the queue
        let items = backend
            .queue_get(&queue_name, 2, QueueDirection::Front)
            .unwrap();
        assert_eq!(items, vec!["item3".to_string(), "item2".to_string()]); // Insertion is to the front

        // Retrieve items from the queue from back
        let items = backend
            .queue_get(&queue_name, 2, QueueDirection::Back)
            .unwrap();
        assert_eq!(items, vec!["item1".to_string(), "item2".to_string()]); // Insertion is to the front

        clean(&queue_name);
    }

    #[test]
    fn test_queue_count() {
        let queue_name = Uuid::new_v4().to_string();
        let backend = init_redis();

        // Initially, the queue should be empty
        let count = backend.queue_count(&queue_name).unwrap();
        assert_eq!(count, 0);

        // Push some items and check the count
        backend.queue_push(&queue_name, "item1").unwrap();
        backend.queue_push(&queue_name, "item2").unwrap();
        let count = backend.queue_count(&queue_name).unwrap();
        assert_eq!(count, 2);

        clean(&queue_name)
    }

    #[test]
    fn test_storage_upsert_and_get() {
        let backend = init_redis();
        let hash_name = "test_hash";
        let key = "key1";
        let value = "value1";

        // Upsert a key-value pair into the storage
        let result = backend.storage_upsert(hash_name, key, value.to_string());
        assert!(result.is_ok());

        // Get the value back from storage
        let stored_value = backend.storage_get(hash_name, key).unwrap();
        assert_eq!(stored_value, Some(value.to_string()));
    }

    #[test]
    fn test_storage_get_non_existent_key() {
        let backend = init_redis();

        let hash_name = "test_hash";
        let key = "non_existent_key";

        // Try to get a non-existent key from the storage
        let stored_value = backend.storage_get(hash_name, key).unwrap();
        assert_eq!(stored_value, None);
    }
}
