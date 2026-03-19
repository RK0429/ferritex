use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDocumentBuffer {
    pub uri: String,
    pub language_id: String,
    pub version: i32,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct OpenDocumentStore {
    documents: HashMap<String, OpenDocumentBuffer>,
}

impl OpenDocumentStore {
    pub fn open(&mut self, document: OpenDocumentBuffer) {
        self.documents.insert(document.uri.clone(), document);
    }

    pub fn update(&mut self, uri: &str, version: i32, text: String) -> Option<()> {
        let document = self.documents.get_mut(uri)?;
        document.version = version;
        document.text = text;
        Some(())
    }

    pub fn close(&mut self, uri: &str) -> Option<OpenDocumentBuffer> {
        self.documents.remove(uri)
    }

    pub fn get(&self, uri: &str) -> Option<&OpenDocumentBuffer> {
        self.documents.get(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenDocumentBuffer, OpenDocumentStore};

    #[test]
    fn stores_and_updates_open_documents() {
        let mut store = OpenDocumentStore::default();
        store.open(OpenDocumentBuffer {
            uri: "file:///main.tex".to_string(),
            language_id: "latex".to_string(),
            version: 1,
            text: "hello".to_string(),
        });

        store
            .update("file:///main.tex", 2, "updated".to_string())
            .expect("document exists");

        let document = store.get("file:///main.tex").expect("stored");
        assert_eq!(document.version, 2);
        assert_eq!(document.text, "updated");
    }

    #[test]
    fn closes_documents() {
        let mut store = OpenDocumentStore::default();
        store.open(OpenDocumentBuffer {
            uri: "file:///main.tex".to_string(),
            language_id: "latex".to_string(),
            version: 1,
            text: "hello".to_string(),
        });

        let closed = store.close("file:///main.tex").expect("closed document");

        assert_eq!(closed.uri, "file:///main.tex");
        assert!(store.get("file:///main.tex").is_none());
    }
}
