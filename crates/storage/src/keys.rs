pub struct ObjectKey<'a> {
    pub kind: &'a str,
    pub owner: &'a str,
    pub id: &'a str,
    pub file: &'a str,
}

#[allow(clippy::inherent_to_string)]
impl<'a> ObjectKey<'a> {
    pub fn new(kind: &'a str, owner: &'a str, id: &'a str, file: &'a str) -> Self {
        Self {
            kind,
            owner,
            id,
            file,
        }
    }

    pub fn to_string(&self) -> String {
        format!("{}/{}/{}/{}", self.kind, self.owner, self.id, self.file)
    }
}

#[cfg(test)]
mod tests {
    use super::ObjectKey;

    #[test]
    fn builds_canonical_key() {
        let key = ObjectKey::new("media", "alice", "abc123", "song.mp3");
        assert_eq!(key.to_string(), "media/alice/abc123/song.mp3");
    }
}
