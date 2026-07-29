use sha2::{Digest, Sha256};

use crate::{
    error::{Error, Result},
    model::BlobEntry,
};

pub(super) fn parse_index(bytes: &[u8]) -> Result<Vec<BlobEntry>> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::Cloud("cloud index is not UTF-8".into()))?;
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty() && !line.starts_with("0:."))
        .map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            if fields.len() < 5 {
                return Err(Error::Cloud("malformed cloud index".into()));
            }
            Ok(BlobEntry {
                hash: fields[0].into(),
                id: fields[2].into(),
                subfiles: fields[3]
                    .parse()
                    .map_err(|_| Error::Cloud("invalid subfile count".into()))?,
                size: fields[4]
                    .parse()
                    .map_err(|_| Error::Cloud("invalid blob size".into()))?,
            })
        })
        .collect()
}

pub(super) fn serialize_document_index(entries: &[BlobEntry]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.id.clone());
    let mut output = String::from("3\n");
    for entry in sorted {
        output.push_str(&format!("{}:0:{}:0:{}\n", entry.hash, entry.id, entry.size));
    }
    output.into_bytes()
}

pub(super) fn serialize_root_index(entries: &[BlobEntry]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.id.clone());
    let total = sorted.iter().map(|entry| entry.size).sum::<u64>();
    let mut output = format!("4\n0:.:{}:{}\n", sorted.len(), total);
    for entry in sorted {
        output.push_str(&format!(
            "{}:0:{}:{}:{}\n",
            entry.hash, entry.id, entry.subfiles, entry.size
        ));
    }
    output.into_bytes()
}

pub(super) fn hash_entries(entries: &[BlobEntry]) -> Result<String> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.id.clone());
    let mut hasher = Sha256::new();
    for entry in sorted {
        hasher.update(hex_decode(&entry.hash)?);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn hex_decode(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(Error::Cloud("invalid hash length".into()));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| Error::Cloud("invalid content hash".into()))
        })
        .collect()
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cloud_index() {
        let entries =
            parse_index(b"3\naabb:0:doc.metadata:0:12\nccdd:0:doc.content:0:7\n").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "doc.metadata");
        assert_eq!(entries[1].size, 7);
    }

    #[test]
    fn root_serialization_is_stable() {
        let entries = vec![
            BlobEntry {
                hash: "bb".repeat(32),
                id: "b".into(),
                subfiles: 2,
                size: 20,
            },
            BlobEntry {
                hash: "aa".repeat(32),
                id: "a".into(),
                subfiles: 1,
                size: 10,
            },
        ];
        assert_eq!(
            String::from_utf8(serialize_root_index(&entries)).unwrap(),
            format!(
                "4\n0:.:2:30\n{}:0:a:1:10\n{}:0:b:2:20\n",
                "aa".repeat(32),
                "bb".repeat(32)
            )
        );
    }
}
