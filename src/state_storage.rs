use std::{marker::PhantomData, sync::Arc};

use anyhow::Result;
use bincode::Options;
use dirs::home_dir;
use rocksdb::{DBWithThreadMode, SingleThreaded, DB};
use serde::{de::DeserializeOwned, Serialize};
use tracing::error;

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone)]
pub enum StorageType {
    PPLNS,
}

impl StorageType {
    pub fn prefix(&self) -> &'static [u8; 1] {
        match self {
            StorageType::PPLNS => &[0],
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
        let db_path = home.unwrap().join(".aleo_pool_testnet3/state.db");
        let mut db_options = rocksdb::Options::default();
        db_options.create_if_missing(true);
        db_options.set_compression_type(rocksdb::DBCompressionType::Zstd);
        db_options.set_use_fsync(true);
        db_options.set_prefix_extractor(rocksdb::SliceTransform::create_fixed_prefix(1));
        db_options.set_comparator("comparator_v1", |a, b| {
            if !(a[0] == 1 && b[0] == 1) {
                a.cmp(b)
            } else if a == [1] {
                std::cmp::Ordering::Less
            } else if b == [1] {
                std::cmp::Ordering::Greater
            } else {
                a.cmp(b).reverse()
            }
        });

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
        let options = bincode::config::DefaultOptions::new()
            .with_big_endian()
            .with_fixint_encoding()
            .allow_trailing_bytes();
        let mut key_buf = vec![self.storage_type.prefix()[0]];
        key_buf.reserve(options.serialized_size(&key)? as usize);
        options.serialize_into(&mut key_buf, key)?;
        match self.db.get(key_buf)? {
            Some(value) => Ok(Some(options.deserialize(&value)?)),
            None => Ok(None),
        }
    }

    pub fn put(&self, key: &K, value: &V) -> Result<()> {
        let options = bincode::config::DefaultOptions::new()
            .with_big_endian()
            .with_fixint_encoding()
            .allow_trailing_bytes();
        let mut key_buf = vec![self.storage_type.prefix()[0]];
        key_buf.reserve(options.serialized_size(&key)? as usize);
        options.serialize_into(&mut key_buf, key)?;
        let value_buf = options.serialize(value)?;
        self.db.put(key_buf, value_buf)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn iter(&self) -> StorageIter<'_, K, V> {
        StorageIter {
            storage_type: self.storage_type.clone(),
            iter: self.db.prefix_iterator(self.storage_type.prefix()),
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
        let options = bincode::config::DefaultOptions::new()
            .with_big_endian()
            .with_fixint_encoding()
            .allow_trailing_bytes();
        match self.iter.next()? {
            Ok((raw_key, raw_value)) => {
                if raw_key[0] == self.storage_type.prefix()[0] {
                    let key = options.deserialize::<K>(&raw_key[1..]);
                    let value = options.deserialize::<V>(&raw_value);
                    match key.and_then(|k| value.map(|v| (k, v))) {
                        Ok(item) => Some(item),
                        Err(e) => {
                            error!("Failed to deserialize key or value: {:?}", e);
                            error!("Key: {:?}", &raw_key);
                            error!("Value: {:?}", &raw_value);
                            None
                        }
                    }
                } else {
                    None
                }
            }
            Err(e) => {
                error!("Failed to iterate: {:?}", e);
                None
            }
        }
    }
}
