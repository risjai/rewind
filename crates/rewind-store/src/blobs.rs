use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Content-addressed blob store (like git objects).
/// Files are stored at: {root}/{first 2 chars of hash}/{remaining hash}
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o700));
        }
        Ok(BlobStore {
            root: root.to_path_buf(),
        })
    }

    /// Store data and return its SHA-256 hash
    pub fn put(&self, data: &[u8]) -> Result<String> {
        let hash = Self::hash(data);
        let path = self.blob_path(&hash);

        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, data)?;
        }

        Ok(hash)
    }

    /// Retrieve data by hash
    pub fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.blob_path(hash);
        Ok(fs::read(&path)?)
    }

    /// Check if a blob exists
    pub fn exists(&self, hash: &str) -> bool {
        self.blob_path(hash).exists()
    }

    /// Store a JSON-serializable value
    pub fn put_json<T: serde::Serialize>(&self, value: &T) -> Result<String> {
        let data = serde_json::to_vec(value)?;
        self.put(&data)
    }

    /// Retrieve and deserialize a JSON blob
    pub fn get_json<T: serde::de::DeserializeOwned>(&self, hash: &str) -> Result<T> {
        let data = self.get(hash)?;
        Ok(serde_json::from_slice(&data)?)
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        if hash.len() < 3 {
            return self.root.join("_invalid").join(hash);
        }
        let (prefix, rest) = hash.split_at(2);
        self.root.join(prefix).join(rest)
    }

    fn hash(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }
}
