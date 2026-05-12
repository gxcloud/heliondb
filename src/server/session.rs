use crate::error::Result;

pub struct Session;

impl Session {
    pub fn new() -> Self {
        Session
    }

    pub async fn handle_query(&self, _query: &str) -> Result<String> {
        Ok("OK".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_handle() {
        let session = Session::new();
        let result = session.handle_query("SELECT 1").await.unwrap();
        assert_eq!(result, "OK");
    }
}
