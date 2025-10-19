# temps-embeddings

A flexible, trait-based embedding service for creating vector representations from text.

## Features

- **Trait-based abstraction** - Easy to swap tokenization and embedding strategies
- **Multiple implementations** - Hash-based and TF-IDF embedders included
- **Batch processing** - Efficient batch operations for multiple texts
- **PostgreSQL integration** - Direct support for pgvector type
- **Low CPU usage** - Optimized for production use

## Quick Start

```rust
use temps_embeddings::{EmbeddingService, HashEmbedder};

// Create an embedder
let embedder = HashEmbedder::new(10000, 384);
let service = EmbeddingService::new(Box::new(embedder));

// Create embedding
let embedding = service.create_embedding("Database connection failed")?;

// Use with error groups
let mut active_model: error_groups::ActiveModel = error.into();
active_model.embedding = Set(Some(embedding));
active_model.update(db).await?;
```

## Using the Builder

```rust
use temps_embeddings::EmbeddingServiceBuilder;

let service = EmbeddingServiceBuilder::new()
    .vocab_size(10000)
    .embedding_dim(384)
    .with_hash_embedder()
    .build()?;

let embedding = service.create_embedding("Error message")?;
```

## Custom Tokenizers

Implement the `Tokenizer` trait to create custom tokenization strategies:

```rust
use temps_embeddings::tokenizer::{Tokenizer, TokenizerResult};

struct MyCustomTokenizer;

impl Tokenizer for MyCustomTokenizer {
    fn encode(&self, text: &str) -> TokenizerResult<Vec<u32>> {
        // Your tokenization logic
        todo!()
    }

    fn decode(&self, token_ids: &[u32]) -> TokenizerResult<String> {
        // Your decoding logic
        todo!()
    }

    fn vocab_size(&self) -> usize {
        10000
    }

    fn name(&self) -> &str {
        "MyCustomTokenizer"
    }
}
```

## Custom Embedders

Implement the `Embedder` trait to create custom embedding strategies:

```rust
use temps_embeddings::embedder::{Embedder, EmbedderResult};
use temps_entities::error_groups::PgVector;

struct MyCustomEmbedder {
    embedding_dim: usize,
}

impl Embedder for MyCustomEmbedder {
    fn embed(&self, text: &str) -> EmbedderResult<PgVector> {
        // Your embedding logic
        todo!()
    }

    fn embedding_dim(&self) -> usize {
        self.embedding_dim
    }

    fn name(&self) -> &str {
        "MyCustomEmbedder"
    }
}
```

## Available Embedders

### HashEmbedder

Fast, hash-based embedding using token frequency features.

**Pros:**
- Very fast
- No training required
- Consistent results

**Cons:**
- Less semantic meaning than ML models
- Hash collisions possible

**Use for:** Production error grouping, real-time similarity search

### TfIdfEmbedder

TF-IDF weighted embeddings for better semantic understanding.

**Pros:**
- Better semantic representation
- Weighs important terms higher

**Cons:**
- Requires corpus for IDF computation
- Slightly slower

**Use for:** Document classification, semantic search with known corpus

## Batch Processing

Process multiple texts efficiently:

```rust
let texts = vec![
    "Database error 1",
    "Database error 2",
    "API error",
];

let embeddings = service.create_embeddings(&texts)?;
```

## Integration Example

```rust
use temps_embeddings::{EmbeddingService, HashEmbedder};
use temps_entities::error_groups;
use sea_orm::{ActiveModelTrait, Set};

async fn create_error_with_embedding(
    db: &DatabaseConnection,
    error_message: &str,
    service: &EmbeddingService,
) -> Result<error_groups::Model, Box<dyn std::error::Error>> {
    // Create embedding
    let embedding = service.create_error_embedding(error_message)?;

    // Create error group
    let error = error_groups::ActiveModel {
        title: Set("Error occurred".to_string()),
        message_template: Set(Some(error_message.to_string())),
        embedding: Set(Some(embedding)),
        // ... other fields
        ..Default::default()
    };

    let result = error.insert(db).await?;
    Ok(result)
}
```

## Architecture

```
EmbeddingService
    ↓
Embedder (trait)
    ↓
Tokenizer (trait)
    ↓
PgVector (database type)
```

## Performance

- **Hash tokenization**: ~1μs per text
- **Embedding creation**: ~5μs for 384-dim vectors
- **Batch processing**: 2-3x faster than individual calls
- **Memory**: Minimal overhead, no large model loading

## Testing

```bash
cargo test -p temps-embeddings
```

## Future Enhancements

- [ ] Neural network-based embedders
- [ ] Pre-trained model support (BERT, etc.)
- [ ] Caching layer for frequently embedded texts
- [ ] Async batch processing
- [ ] Multi-language tokenization
