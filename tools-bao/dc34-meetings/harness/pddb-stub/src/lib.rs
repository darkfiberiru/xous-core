//! In-memory stand-in for the subset of the PDDB API used by the meeting code.
//!
//! It deliberately copies two behaviours of the real thing that the firmware has to cope with:
//! writing to an existing key overwrites from offset 0 without truncating, and operations on a
//! missing dictionary or key fail with `ErrorKind::NotFound`.

use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

type Dict = BTreeMap<String, Vec<u8>>;

fn store() -> MutexGuard<'static, BTreeMap<String, Dict>> {
    static STORE: OnceLock<Mutex<BTreeMap<String, Dict>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(BTreeMap::new())).lock().unwrap()
}

fn not_found(what: &str) -> Error { Error::new(ErrorKind::NotFound, what.to_owned()) }

pub struct Pddb {}

pub struct PddbKeyRecord {
    pub name: String,
    pub len: usize,
    pub basis: String,
    pub data: Option<Vec<u8>>,
}

pub struct PddbKey<'a> {
    dict: String,
    key: String,
    pos: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl Read for PddbKey<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let store = store();
        let data = store.get(&self.dict).and_then(|d| d.get(&self.key)).ok_or(not_found("key"))?;
        let n = buf.len().min(data.len().saturating_sub(self.pos));
        buf[..n].copy_from_slice(&data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for PddbKey<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let mut store = store();
        let data = store.get_mut(&self.dict).and_then(|d| d.get_mut(&self.key)).ok_or(not_found("key"))?;
        if data.len() < self.pos + buf.len() {
            data.resize(self.pos + buf.len(), 0);
        }
        data[self.pos..self.pos + buf.len()].copy_from_slice(buf);
        self.pos += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl Pddb {
    pub fn new() -> Self { Pddb {} }

    pub fn get(
        &self,
        dict_name: &str,
        key_name: &str,
        _basis_name: Option<&str>,
        create_dict: bool,
        create_key: bool,
        _alloc_hint: Option<usize>,
        _key_changed_cb: Option<impl Fn() + 'static + Send>,
    ) -> Result<PddbKey<'_>> {
        // same limit as the real PDDB (KEY_NAME_LEN - 1)
        if key_name.len() > 94 {
            return Err(Error::new(ErrorKind::InvalidInput, "key name too long"));
        }
        let mut store = store();
        if !store.contains_key(dict_name) {
            if !create_dict {
                return Err(not_found("dictionary"));
            }
            store.insert(dict_name.to_owned(), Dict::new());
        }
        let dict = store.get_mut(dict_name).unwrap();
        if !dict.contains_key(key_name) {
            if !create_key {
                return Err(not_found("key"));
            }
            dict.insert(key_name.to_owned(), Vec::new());
        }
        Ok(PddbKey {
            dict: dict_name.to_owned(),
            key: key_name.to_owned(),
            pos: 0,
            _marker: std::marker::PhantomData,
        })
    }

    pub fn delete_key(&self, dict_name: &str, key_name: &str, _basis_name: Option<&str>) -> Result<()> {
        let mut store = store();
        let dict = store.get_mut(dict_name).ok_or(not_found("dictionary"))?;
        dict.remove(key_name).map(|_| ()).ok_or(not_found("key"))
    }

    pub fn delete_key_list(
        &self,
        dict_name: &str,
        key_list: Vec<String>,
        _basis_name: Option<&str>,
    ) -> Result<()> {
        // the real call packs length-prefixed names into a 3800-byte message (MAX_PDDB_DELETE_LEN)
        if key_list.iter().map(|k| k.len() + 1).sum::<usize>() >= 3800 {
            return Err(Error::new(ErrorKind::OutOfMemory, "Key list total size exceeds delete buffer"));
        }
        let mut store = store();
        let dict = store.get_mut(dict_name).ok_or(not_found("dictionary"))?;
        for key in key_list {
            dict.remove(&key);
        }
        Ok(())
    }

    pub fn list_keys(&self, dict_name: &str, _basis_name: Option<&str>) -> Result<Vec<String>> {
        let store = store();
        Ok(store.get(dict_name).ok_or(not_found("dictionary"))?.keys().cloned().collect())
    }

    pub fn read_dict(
        &self,
        dict: &str,
        _basis: Option<&str>,
        size_limit: Option<usize>,
    ) -> Result<Vec<PddbKeyRecord>> {
        let store = store();
        let mut budget = size_limit.unwrap_or(usize::MAX);
        Ok(store
            .get(dict)
            .ok_or(not_found("dictionary"))?
            .iter()
            .map(|(name, data)| {
                // like the real bulk read, records past the size budget come back without data
                let fits = data.len() <= budget;
                if fits {
                    budget -= data.len();
                }
                PddbKeyRecord {
                    name: name.clone(),
                    len: data.len(),
                    basis: String::from(".System"),
                    data: if fits && !data.is_empty() { Some(data.clone()) } else { None },
                }
            })
            .collect())
    }

    pub fn sync(&self) -> Result<()> { Ok(()) }

    /// Test hook: plant raw bytes, bypassing all validation, to simulate a corrupted record.
    pub fn raw_put(dict: &str, key: &str, data: &[u8]) {
        store().entry(dict.to_owned()).or_default().insert(key.to_owned(), data.to_vec());
    }
}
