use std::{marker::PhantomData, sync::Arc};

use anyhow::Result;
use dirs::home_dir;
use rocksdb::{BlockBasedOptions, Options, DB};
use serde::{de::DeserializeOwned, Serialize};

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
}
