//! Example usage of the tokenization abstraction with error groups
//!
//! Run with: cargo run --example tokenization_usage

use temps_entities::error_groups::{Model as ErrorGroup, PgVector};
use temps_entities::tokenizer::{CharTokenizer, HashTokenizer, SimpleTokenizer, Tokenizer};

fn main() {
    println!("=== Error Message Tokenization Examples ===\n");

    // Create a sample error group
    let error = create_sample_error();

    // Example 1: Using Hash Tokenizer (recommended for production)
    println!("1. Hash Tokenizer (production-ready, low CPU):");
    let hash_tokenizer = HashTokenizer::new(10000);
    demonstrate_tokenization(&error, &hash_tokenizer, 384);

    // Example 2: Using Simple Tokenizer with vocabulary
    println!("\n2. Simple Tokenizer (with predefined vocabulary):");
    let vocab = vec![
        "error".to_string(),
        "database".to_string(),
        "connection".to_string(),
        "failed".to_string(),
    ];
    let simple_tokenizer = SimpleTokenizer::with_vocab(vocab);
    demonstrate_tokenization(&error, &simple_tokenizer, 384);

    // Example 3: Using Character Tokenizer (maximum detail)
    println!("\n3. Character Tokenizer (high granularity):");
    let char_tokenizer = CharTokenizer::new();
    demonstrate_tokenization(&error, &char_tokenizer, 384);

    // Example 4: Batch tokenization
    println!("\n4. Batch Tokenization (efficient for multiple errors):");
    demonstrate_batch_tokenization(&hash_tokenizer);

    // Example 5: Creating embeddings for similarity search
    println!("\n5. Creating Embeddings for Similarity Search:");
    demonstrate_embedding_creation(&hash_tokenizer);
}

fn create_sample_error() -> ErrorGroup {
    use chrono::Utc;

    ErrorGroup {
        id: 1,
        title: "Database connection failed".to_string(),
        error_type: "ConnectionError".to_string(),
        message_template: Some("Failed to connect to database: timeout after 30s".to_string()),
        embedding: None,
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        total_count: 1,
        status: "unresolved".to_string(),
        assigned_to: None,
        project_id: 1,
        environment_id: Some(1),
        deployment_id: Some(1),
        visitor_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn demonstrate_tokenization(error: &ErrorGroup, tokenizer: &dyn Tokenizer, embedding_size: usize) {
    // Tokenize the error message
    match error.tokenize_message(tokenizer) {
        Ok(tokens) => {
            println!("   Tokens: {:?}", tokens);
            println!("   Token count: {}", tokens.len());
            println!("   Vocab size: {}", tokenizer.vocab_size());

            // Create embedding
            match error.tokenize_and_embed(tokenizer, embedding_size) {
                Ok(embedding) => {
                    println!("   Embedding size: {}", embedding.0.len());
                    println!(
                        "   Embedding (first 10): {:?}",
                        &embedding.0[..10.min(embedding.0.len())]
                    );
                }
                Err(e) => println!("   Error creating embedding: {}", e),
            }
        }
        Err(e) => println!("   Error tokenizing: {}", e),
    }
}

fn demonstrate_batch_tokenization(tokenizer: &HashTokenizer) {
    let messages = vec![
        "Database connection timeout",
        "API request failed with 500",
        "Memory allocation error",
    ];

    match tokenizer.encode_batch(&messages) {
        Ok(batch_tokens) => {
            println!("   Successfully tokenized {} messages", batch_tokens.len());
            for (i, tokens) in batch_tokens.iter().enumerate() {
                println!("   Message {}: {} tokens", i + 1, tokens.len());
            }
        }
        Err(e) => println!("   Error in batch tokenization: {}", e),
    }
}

fn demonstrate_embedding_creation(tokenizer: &HashTokenizer) {
    let error1 = "Database connection failed";
    let error2 = "Database connection timeout";
    let error3 = "API request error";

    let embedding1 = create_embedding(tokenizer, error1, 384);
    let embedding2 = create_embedding(tokenizer, error2, 384);
    let embedding3 = create_embedding(tokenizer, error3, 384);

    println!(
        "   Similarity (error1 vs error2): {:.4}",
        cosine_similarity(&embedding1, &embedding2)
    );
    println!(
        "   Similarity (error1 vs error3): {:.4}",
        cosine_similarity(&embedding1, &embedding3)
    );
    println!("   Note: Higher similarity means errors are more similar");
}

fn create_embedding(tokenizer: &HashTokenizer, text: &str, size: usize) -> PgVector {
    let tokens = tokenizer.encode(text).unwrap();
    ErrorGroup::create_embedding_from_tokens(&tokens, size)
}

fn cosine_similarity(a: &PgVector, b: &PgVector) -> f32 {
    let dot_product: f32 = a.0.iter().zip(b.0.iter()).map(|(x, y)| x * y).sum();
    let magnitude_a: f32 = a.0.iter().map(|x| x * x).sum::<f32>().sqrt();
    let magnitude_b: f32 = b.0.iter().map(|x| x * x).sum::<f32>().sqrt();

    if magnitude_a == 0.0 || magnitude_b == 0.0 {
        0.0
    } else {
        dot_product / (magnitude_a * magnitude_b)
    }
}
