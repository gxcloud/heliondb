use crate::error::Result;
use crate::storage::engine::DatabaseEngine;

pub enum PhysicalOp {
    CreateTable { name: String },
    DropTable { name: String },
    Insert { table: String },
    Select { table: String },
    Update { table: String },
    Delete { table: String },
}

pub async fn execute(
    _engine: &DatabaseEngine,
    op: &PhysicalOp,
) -> Result<()> {
    match op {
        PhysicalOp::CreateTable { name } => {
            let _ = name;
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execute_placeholder() {
        let dir = TempDir::new().unwrap();
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        let op = PhysicalOp::CreateTable { name: "test".to_string() };
        let result = execute(&engine, &op).await;
        assert!(result.is_ok());
    }
}
