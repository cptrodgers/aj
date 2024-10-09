//! WARNING: Mem is purposing for testing or small application. 
//! It doesn't support Persistent Mode.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::types::{Backend, QueueDirection};
use crate::Error;

#[derive(Default)]
pub struct InMemory {
    pub queues: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
    pub storages: Arc<Mutex<HashMap<String, HashMap<String, String>>>>,
}

fn insert_item_to_queue(queue: &mut VecDeque<String>, item: String, direction: QueueDirection) {
    match direction {
        QueueDirection::Front => queue.push_front(item),
        QueueDirection::Back => queue.push_back(item),
    }
}

impl Backend for InMemory {
    fn queue_push(&self, queue_name: &str, item: &str) -> Result<(), Error> {
        let mut queues = self.queues.lock().unwrap();
        if let Some(queue) = queues.get_mut(queue_name) {
            insert_item_to_queue(queue, item.into(), QueueDirection::Front);
        } else {
            let items = VecDeque::from([item.to_string()]);
            queues.insert(queue_name.to_string(), items);
        }
        Ok(())
    }

    fn queue_move(
        &self,
        from_queue: &str,
        to_queue: &str,
        count: usize,
        from_direction: QueueDirection,
        to_direction: QueueDirection,
    ) -> Result<Vec<String>, Error> {
        let mut queues = self.queues.lock().unwrap();
        let mut moving_items = vec![];
        if let Some(from_queue) = queues.get_mut(from_queue) {
            for _ in 0..count {
                let item = match from_direction {
                    QueueDirection::Front => from_queue.pop_front(),
                    QueueDirection::Back => from_queue.pop_back(),
                };
                if let Some(item) = item {
                    moving_items.push(item);
                }
            }
        }

        for moving_item in moving_items.iter() {
            if let Some(to_queue) = queues.get_mut(to_queue) {
                insert_item_to_queue(to_queue, moving_item.clone(), to_direction);
            } else {
                let items = VecDeque::from([moving_item.clone()]);
                queues.insert(to_queue.to_string(), items);
            }
        }

        Ok(moving_items)
    }

    fn queue_remove(&self, queue_name: &str, item: &str) -> Result<(), Error> {
        if let Some(queue) = self.queues.lock().unwrap().get_mut(queue_name) {
            queue.retain(|inner_item| inner_item.as_str() != item);
        }

        Ok(())
    }

    fn queue_get(&self, queue: &str, count: usize) -> Result<Vec<String>, Error> {
        let mut items = vec![];
        if let Some(queue) = self.queues.lock().unwrap().get(queue) {
            for index in 0..count {
                if let Some(item) = queue.get(index) {
                    items.push(item.to_string());
                }
            }
        }

        Ok(items)
    }

    fn queue_count(&self, queue: &str) -> Result<usize, Error> {
        if let Some(queue) = self.queues.lock().unwrap().get(queue) {
            Ok(queue.len())
        } else {
            Ok(0)
        }
    }

    fn storage_upsert(&self, hash: &str, key: &str, value: String) -> Result<(), Error> {
        let mut storages = self.storages.lock().unwrap();
        if let Some(storage) = storages.get_mut(hash) {
            storage.insert(key.to_string(), value);
        } else {
            let mut inner_storage = HashMap::new();
            inner_storage.insert(key.to_string(), value);
            storages.insert(hash.to_string(), inner_storage);
        }
        Ok(())
    }

    fn storage_get(&self, hash: &str, key: &str) -> Result<Option<String>, Error> {
        let storages = self.storages.lock().unwrap();
        if let Some(storage) = storages.get(hash) {
            Ok(storage.get(key).cloned())
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::QueueDirection;

    #[test]
    fn test_queue_push() {
        let backend = InMemory::default();
        let queue_name = "test_queue";

        // Push an item to the queue
        let result = backend.queue_push(queue_name, "item1");
        assert!(result.is_ok());

        // Check that the queue contains the pushed item
        let items = backend.queue_get(queue_name, 1).unwrap();
        assert_eq!(items, vec!["item1".to_string()]);

        // Push another item and check
        backend.queue_push(queue_name, "item2").unwrap();
        let items = backend.queue_get(queue_name, 2).unwrap();
        assert_eq!(items, vec!["item2".to_string(), "item1".to_string()]); // Inserting to front
    }

    #[test]
    fn test_queue_move() {
        let backend = InMemory::default();
        let from_queue = "from_queue";
        let to_queue = "to_queue";

        // Push items to the from_queue
        backend.queue_push(from_queue, "item1").unwrap();
        backend.queue_push(from_queue, "item2").unwrap();

        // Move one item from the front of from_queue to the back of to_queue
        let result = backend.queue_move(from_queue, to_queue, 1, QueueDirection::Front, QueueDirection::Back);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), vec!["item2".to_string()]); // item2 is at the front

        // Check that to_queue now contains the moved item
        let items = backend.queue_get(to_queue, 1).unwrap();
        assert_eq!(items, vec!["item2".to_string()]);
    }

    #[test]
    fn test_queue_remove() {
        let backend = InMemory::default();
        let queue_name = "test_queue";

        // Push items to the queue
        backend.queue_push(queue_name, "item1").unwrap();
        backend.queue_push(queue_name, "item2").unwrap();

        // Remove an item from the queue
        let result = backend.queue_remove(queue_name, "item1");
        assert!(result.is_ok());

        // Ensure the item was removed
        let items = backend.queue_get(queue_name, 10).unwrap();
        assert_eq!(items, vec!["item2".to_string()]);
    }

    #[test]
    fn test_queue_get() {
        let backend = InMemory::default();
        let queue_name = "test_queue";

        // Push multiple items to the queue
        backend.queue_push(queue_name, "item1").unwrap();
        backend.queue_push(queue_name, "item2").unwrap();
        backend.queue_push(queue_name, "item3").unwrap();

        // Retrieve items from the queue
        let items = backend.queue_get(queue_name, 2).unwrap();
        assert_eq!(items, vec!["item3".to_string(), "item2".to_string()]); // Insertion is to the front
    }

    #[test]
    fn test_queue_count() {
        let backend = InMemory::default();
        let queue_name = "test_queue";

        // Initially, the queue should be empty
        let count = backend.queue_count(queue_name).unwrap();
        assert_eq!(count, 0);

        // Push some items and check the count
        backend.queue_push(queue_name, "item1").unwrap();
        backend.queue_push(queue_name, "item2").unwrap();
        let count = backend.queue_count(queue_name).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_storage_upsert_and_get() {
        let backend = InMemory::default();
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
        let backend = InMemory::default();
        let hash_name = "test_hash";
        let key = "non_existent_key";

        // Try to get a non-existent key from the storage
        let stored_value = backend.storage_get(hash_name, key).unwrap();
        assert_eq!(stored_value, None);
    }
}
