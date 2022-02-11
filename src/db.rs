use std::{marker::PhantomData, sync::Arc};

use anyhow::Result;
use dirs::home_dir;
use rocksdb::{BlockBasedOptions, DBWithThreadMode, Direction, Options, SingleThreaded, DB};
use serde::{de::DeserializeOwned, Serialize};
use tracing::error;

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone)]
pub enum StorageType {
    PPLNS,
    BlockSnapshot,
}

impl StorageType {
    pub fn prefix(&self) -> &'static [u8; 1] {
        match self {
            StorageType::PPLNS => b"\x00",
            StorageType::BlockSnapshot => b"\x01",
        }
    }
}

pub struct Storage {
    db: Arc<DB>,
}

impl Storage {
    pub fn load() -> Storage {
        let home = home_dir();
        if home.is_none() {
            panic!("No home directory found");
        }
        let db_path = home.unwrap().join(".aleo_pool/accounting.db");
        let mut db_options = Options::default();
        db_options.create_if_missing(true);
        db_options.set_compression_type(rocksdb::DBCompressionType::Zstd);
        db_options.set_use_fsync(true);
        db_options.set_prefix_extractor(rocksdb::SliceTransform::create_fixed_prefix(1));
        let mut table_options = BlockBasedOptions::default();
        table_options.set_bloom_filter(10, false);
        db_options.set_block_based_table_factory(&table_options);

        let db = DB::open(&db_options, db_path.to_str().unwrap()).expect("Failed to open DB");

        Storage { db: Arc::new(db) }
    }

    pub fn init_data<K: Serialize + DeserializeOwned, V: Serialize + DeserializeOwned>(
        &self,
        storage_type: StorageType,
    ) -> StorageData<K, V> {
        StorageData {
            db: self.db.clone(),
            storage_type,
            _p: PhantomData,
        }
    }
}

#[derive(Clone)]
pub struct StorageData<K: Serialize + DeserializeOwned, V: Serialize + DeserializeOwned> {
    db: Arc<DB>,
    storage_type: StorageType,
    _p: PhantomData<(K, V)>,
}

impl<K: Serialize + DeserializeOwned, V: Serialize + DeserializeOwned> StorageData<K, V> {
    pub fn get(&self, key: &K) -> Result<Option<V>> {
        let mut key_buf = vec![self.storage_type.prefix()[0]];
        key_buf.reserve(bincode::serialized_size(&key)? as usize);
        bincode::serialize_into(&mut key_buf, key)?;
        match self.db.get(key_buf)? {
            Some(value) => Ok(Some(bincode::deserialize(&value)?)),
            None => Ok(None),
        }
    }

    pub fn put(&self, key: &K, value: &V) -> Result<()> {
        let mut key_buf = vec![self.storage_type.prefix()[0]];
        key_buf.reserve(bincode::serialized_size(&key)? as usize);
        bincode::serialize_into(&mut key_buf, key)?;
        let value_buf = bincode::serialize(value)?;
        self.db.put(key_buf, value_buf)?;
        Ok(())
    }

    pub fn iter(&self) -> StorageIter<'_, K, V> {
        StorageIter {
            storage_type: self.storage_type.clone(),
            iter: self.db.iterator(rocksdb::IteratorMode::From(
                &*vec![self.storage_type.prefix()[0]],
                Direction::Forward,
            )),
            _p: Default::default(),
        }
    }
}

pub struct StorageIter<'a, K: Serialize + DeserializeOwned, V: Serialize + DeserializeOwned> {
    storage_type: StorageType,
    iter: rocksdb::DBIteratorWithThreadMode<'a, DBWithThreadMode<SingleThreaded>>,
    _p: PhantomData<(K, V)>,
}

impl<'a, K: Serialize + DeserializeOwned, V: Serialize + DeserializeOwned> Iterator for StorageIter<'a, K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.iter.valid() {
            let (raw_key, raw_value) = self.iter.next()?;
            if raw_key[0] == self.storage_type.prefix()[0] {
                let key = bincode::deserialize::<K>(&raw_key[1..]);
                let value = bincode::deserialize::<V>(&raw_value);
                return match key.and_then(|k| value.map(|v| (k, v))) {
                    Ok(item) => Some(item),
                    Err(e) => {
                        error!("Failed to deserialize key or value: {:?}", e);
                        error!("Key: {:?}", &raw_key);
                        error!("Value: {:?}", &raw_value);
                        None
                    }
                };
            }
            None
        } else {
            None
        }
    }
}
