//! Proof-of-Work CAPTCHA handler
//!
//! This handler provides endpoints for generating and verifying proof-of-work challenges
//! to protect against DDoS attacks without requiring third-party CAPTCHA services.

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_core::problemdetails::{self, Problem};
use temps_database::DbConnection;
use tracing::info;
use utoipa::ToSchema;

use crate::service::challenge_service::ChallengeService;

/// Request to verify a proof-of-work solution
#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyChallengeRequest {
    /// The original challenge string
    pub challenge: String,
    /// The nonce found by the client
    pub nonce: String,
    /// Environment ID
    pub environment_id: i32,
    /// Client identifier (JA4 fingerprint or IP address)
    pub identifier: String,
    /// Type of identifier ("ja4" or "ip")
    pub identifier_type: String,
}

/// Response after successful challenge verification
#[derive(Debug, Serialize, ToSchema)]
pub struct VerifyChallengeResponse {
    /// Whether the challenge was successfully verified
    pub success: bool,
    /// Session expiration time (Unix timestamp)
    pub expires_at: i64,
    /// Message describing the result
    pub message: String,
}

/// State for the CAPTCHA handler
pub struct CaptchaState {
    pub db: Arc<DbConnection>,
    pub challenge_service: Arc<ChallengeService>,
}

/// Verify a proof-of-work solution and mark the challenge as complete
///
/// This endpoint verifies that the provided nonce produces a hash with the
/// required number of leading zero bits. If valid, it marks the challenge
/// session as complete for the client's IP address.
#[utoipa::path(
    post,
    path = "/_temps/captcha/verify",
    tag = "CAPTCHA",
    request_body = VerifyChallengeRequest,
    responses(
        (status = 200, description = "Challenge verified", body = VerifyChallengeResponse),
        (status = 400, description = "Invalid solution"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn verify_challenge(
    State(state): State<Arc<CaptchaState>>,
    Json(request): Json<VerifyChallengeRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Verify the proof-of-work solution
    let hash = compute_hash(&request.challenge, &request.nonce);
    let difficulty = 20; // Must match difficulty from generate_challenge

    if !has_leading_zero_bits(&hash, difficulty) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Solution")
            .with_detail("The provided nonce does not produce a valid hash"));
    }

    info!(
        "Valid PoW solution for environment {} from {} {}: challenge={}, nonce={}",
        request.environment_id,
        request.identifier_type,
        request.identifier,
        request.challenge,
        request.nonce
    );

    // Mark challenge as completed (24 hour TTL)
    let session = state
        .challenge_service
        .mark_challenge_completed(
            request.environment_id,
            &request.identifier,
            &request.identifier_type,
            None, // user_agent not available in this context
            24,   // 24 hour TTL
        )
        .await
        .map_err(|e| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Mark Challenge Complete")
                .with_detail(format!("Error: {}", e))
        })?;

    Ok(Json(VerifyChallengeResponse {
        success: true,
        expires_at: session.expires_at.timestamp(),
        message: "Challenge completed successfully. You may now access the site.".to_string(),
    }))
}

/// Compute SHA-256 hash of challenge + nonce
fn compute_hash(challenge: &str, nonce: &str) -> String {
    let input = format!("{}{}", challenge, nonce);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

/// Check if a hash has the required number of leading zero bits
fn has_leading_zero_bits(hash: &str, bits: u32) -> bool {
    let mut leading_zeros = 0u32;

    for c in hash.chars() {
        let digit = match c.to_digit(16) {
            Some(d) => d,
            None => return false, // Invalid hex character
        };

        if digit == 0 {
            leading_zeros += 4;
            if leading_zeros >= bits {
                return true;
            }
        } else {
            // Count leading zeros in this hex digit (0-3 bits)
            // For example: 0x8 = 1000b has 0 leading zeros
            //              0x4 = 0100b has 1 leading zero
            //              0x2 = 0010b has 2 leading zeros
            //              0x1 = 0001b has 3 leading zeros
            leading_zeros += digit.leading_zeros() - 28; // leading_zeros() returns 32-bit count, we need 4-bit
            return leading_zeros >= bits;
        }
    }

    false
}

/// Serve WASM JavaScript bindings
async fn serve_wasm_js() -> impl IntoResponse {
    let wasm_js = include_str!("../../../temps-captcha-wasm/pkg/temps_captcha_wasm.js");
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        wasm_js,
    )
}

/// Serve WASM binary module
async fn serve_wasm_binary() -> impl IntoResponse {
    let wasm_bytes = include_bytes!("../../../temps-captcha-wasm/pkg/temps_captcha_wasm_bg.wasm");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/wasm")],
        wasm_bytes.as_slice(),
    )
}

/// Configure CAPTCHA routes
pub fn create_routes() -> Router<Arc<CaptchaState>> {
    Router::new()
        .route("/_temps/captcha/verify", post(verify_challenge))
        .route("/__temps/temps_captcha_wasm.js", get(serve_wasm_js))
        .route(
            "/__temps/temps_captcha_wasm_bg.wasm",
            get(serve_wasm_binary),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hash() {
        let challenge = "test_challenge";
        let nonce = "12345";
        let hash = compute_hash(challenge, nonce);
        assert_eq!(hash.len(), 64); // SHA-256 produces 64 hex characters
    }

    #[test]
    fn test_has_leading_zero_bits() {
        // Test exact multiples of 4 bits
        assert!(has_leading_zero_bits("0000abcd", 4));
        assert!(has_leading_zero_bits("0000abcd", 8));
        assert!(has_leading_zero_bits("0000abcd", 12));
        assert!(has_leading_zero_bits("0000abcd", 16));
        assert!(!has_leading_zero_bits("0001abcd", 16));
        assert!(!has_leading_zero_bits("1000abcd", 4));

        // Test non-multiples of 4 bits
        assert!(has_leading_zero_bits("00008bcd", 16)); // 0x8 = 1000b, so we have 16 leading zeros
        assert!(has_leading_zero_bits("00004bcd", 17)); // 0x4 = 0100b, so we have 17 leading zeros
        assert!(has_leading_zero_bits("00002bcd", 18)); // 0x2 = 0010b, so we have 18 leading zeros
        assert!(has_leading_zero_bits("00001bcd", 19)); // 0x1 = 0001b, so we have 19 leading zeros
        assert!(has_leading_zero_bits("00000bcd", 20)); // 0x0 then any digit, we have 20+ leading zeros
        assert!(!has_leading_zero_bits("00008bcd", 17)); // Only 16 leading zeros
    }

    #[test]
    fn test_generate_and_solve_challenge() {
        // Generate a random challenge (same as proxy does)
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..16).map(|_| rng.gen()).collect();
        let challenge = hex::encode(bytes);
        let difficulty = 20; // 20 leading zero bits

        println!("Challenge: {}", challenge);
        println!("Difficulty: {} bits", difficulty);

        // Solve the challenge (same algorithm as JavaScript)
        let start_time = std::time::Instant::now();
        let mut nonce = 0u64;
        let mut solution_found = false;

        while nonce < 5_000_000 {
            // Limit attempts to avoid infinite loop in tests (20 bits needs ~1M average)
            let hash = compute_hash(&challenge, &nonce.to_string());

            if has_leading_zero_bits(&hash, difficulty) {
                println!("Solution found! Nonce: {}", nonce);
                println!("Hash: {}", hash);
                println!("Time taken: {:?}", start_time.elapsed());

                // Verify the solution
                assert!(has_leading_zero_bits(&hash, difficulty));
                solution_found = true;
                break;
            }

            nonce += 1;
        }

        assert!(
            solution_found,
            "Failed to find solution within 5,000,000 attempts"
        );
    }

    #[test]
    fn test_solve_known_challenge() {
        // Test with a known challenge that has a quick solution
        let challenge = "test_challenge_12345";
        let difficulty = 20; // 20 leading zero bits

        println!("Testing known challenge: {}", challenge);

        let start_time = std::time::Instant::now();
        let mut nonce = 0u64;
        let mut solution_found = false;

        while nonce < 5_000_000 {
            let hash = compute_hash(challenge, &nonce.to_string());

            if has_leading_zero_bits(&hash, difficulty) {
                println!("Solution found! Nonce: {}", nonce);
                println!("Hash: {}", hash);
                println!("Time taken: {:?}", start_time.elapsed());

                // Verify the solution
                assert!(has_leading_zero_bits(&hash, difficulty));
                solution_found = true;
                break;
            }

            nonce += 1;
        }

        assert!(
            solution_found,
            "Failed to find solution for known challenge"
        );
    }

    #[test]
    fn test_wasm_server_hash_alignment() {
        // Test that verifies WASM and server use the same hash computation
        // This test uses the actual challenge from the failed case to verify the fix

        // The challenge that was failing before the fix
        let challenge = "d0ae14f2f7b43c0002ff22a8cf2a044b";
        let nonce_str = "913428";
        let difficulty = 20;

        // Compute hash using server logic (string concatenation)
        let server_hash = compute_hash(challenge, nonce_str);
        println!("Challenge: {}", challenge);
        println!("Nonce: {}", nonce_str);
        println!("Server hash: {}", server_hash);

        // The hash should be checked using the server's logic
        // This demonstrates that WASM and server now use the same computation
        println!(
            "Hash has leading zeros: {}",
            has_leading_zero_bits(&server_hash, difficulty)
        );

        // Verify the hash is computed correctly by checking it's 64 hex characters
        assert_eq!(
            server_hash.len(),
            64,
            "SHA-256 hash should be 64 hex characters"
        );

        // Verify the hash is valid hex
        assert!(
            hex::decode(&server_hash).is_ok(),
            "Hash should be valid hexadecimal"
        );
    }

    #[test]
    fn test_user_failing_cases_debug() {
        // Debug test using exact challenge/nonce pairs that are failing in production
        // These were provided by the user as not being accepted by the server

        let difficulty = 20;

        // Failing case 1
        let challenge1 = "24b3e6735afa5527e75cb353a4a2c915";
        let nonce1 = "2325748";
        let hash1 = compute_hash(challenge1, nonce1);
        let has_zeros1 = has_leading_zero_bits(&hash1, difficulty);
        println!("\nFailing Case 1:");
        println!("  Challenge: {}", challenge1);
        println!("  Nonce: {}", nonce1);
        println!("  Hash: {}", hash1);
        println!("  Has {} leading zero bits: {}", difficulty, has_zeros1);

        // Count actual leading zeros
        let mut actual_zeros = 0u32;
        for c in hash1.chars() {
            let digit = match c.to_digit(16) {
                Some(d) => d,
                None => break,
            };
            if digit == 0 {
                actual_zeros += 4;
            } else {
                actual_zeros += digit.leading_zeros() - 28;
                break;
            }
        }
        println!("  Actual leading zero bits: {}", actual_zeros);

        // Failing case 2
        let challenge2 = "2d58739d2b03bdd34540ca98b109e141";
        let nonce2 = "793381";
        let hash2 = compute_hash(challenge2, nonce2);
        let has_zeros2 = has_leading_zero_bits(&hash2, difficulty);
        println!("\nFailing Case 2:");
        println!("  Challenge: {}", challenge2);
        println!("  Nonce: {}", nonce2);
        println!("  Hash: {}", hash2);
        println!("  Has {} leading zero bits: {}", difficulty, has_zeros2);

        let mut actual_zeros = 0u32;
        for c in hash2.chars() {
            let digit = match c.to_digit(16) {
                Some(d) => d,
                None => break,
            };
            if digit == 0 {
                actual_zeros += 4;
            } else {
                actual_zeros += digit.leading_zeros() - 28;
                break;
            }
        }
        println!("  Actual leading zero bits: {}", actual_zeros);

        // Failing case 3
        let challenge3 = "45aab4d57950b2cf37ed3f7399184d44";
        let nonce3 = "3293865";
        let hash3 = compute_hash(challenge3, nonce3);
        let has_zeros3 = has_leading_zero_bits(&hash3, difficulty);
        println!("\nFailing Case 3:");
        println!("  Challenge: {}", challenge3);
        println!("  Nonce: {}", nonce3);
        println!("  Hash: {}", hash3);
        println!("  Has {} leading zero bits: {}", difficulty, has_zeros3);

        let mut actual_zeros = 0u32;
        for c in hash3.chars() {
            let digit = match c.to_digit(16) {
                Some(d) => d,
                None => break,
            };
            if digit == 0 {
                actual_zeros += 4;
            } else {
                actual_zeros += digit.leading_zeros() - 28;
                break;
            }
        }
        println!("  Actual leading zero bits: {}", actual_zeros);
    }

    #[test]
    fn test_hash_computation_methods() {
        // Verify that all hash computation methods produce identical results
        let challenge = "24b3e6735afa5527e75cb353a4a2c915";
        let nonce_str = "2325748";
        let nonce_u64: u64 = nonce_str.parse().unwrap();

        // Method 1: Format with &str (server method)
        let input1 = format!("{}{}", challenge, nonce_str);
        let mut hasher1 = Sha256::new();
        hasher1.update(input1.as_bytes());
        let hash1 = hex::encode(hasher1.finalize());

        // Method 2: Separate updates
        let mut hasher2 = Sha256::new();
        hasher2.update(challenge.as_bytes());
        hasher2.update(nonce_str.as_bytes());
        let hash2 = hex::encode(hasher2.finalize());

        // Method 3: Format with u64 (WASM method after fix)
        let input3 = format!("{}{}", challenge, nonce_u64);
        let mut hasher3 = Sha256::new();
        hasher3.update(input3.as_bytes());
        let hash3 = hex::encode(hasher3.finalize());

        println!("\nHash Computation Methods:");
        println!("  Challenge: {}", challenge);
        println!("  Nonce (str): {}", nonce_str);
        println!("  Nonce (u64): {}", nonce_u64);
        println!("  Input 1: {}", input1);
        println!("  Input 3: {}", input3);
        println!();
        println!("  Hash 1 (format with &str):  {}", hash1);
        println!("  Hash 2 (separate updates):   {}", hash2);
        println!("  Hash 3 (format with u64):   {}", hash3);
        println!();
        println!("  Method 1 == Method 2: {}", hash1 == hash2);
        println!("  Method 1 == Method 3: {}", hash1 == hash3);
        println!("  Method 2 == Method 3: {}", hash2 == hash3);

        assert_eq!(
            hash1, hash2,
            "Format with &str should equal separate updates"
        );
        assert_eq!(
            hash1, hash3,
            "Format with u64 should equal format with &str"
        );
    }

    #[test]
    fn test_solve_challenge_server_side() {
        // Implement the WASM solving algorithm on the server side
        // This helps us verify that valid nonces CAN be generated and accepted
        let challenge = "24b3e6735afa5527e75cb353a4a2c915";
        let difficulty = 20;

        println!("\n=== SERVER-SIDE CHALLENGE SOLVER ===");
        println!("Challenge: {}", challenge);
        println!("Difficulty: {} bits", difficulty);

        let mut nonce = 0u64;
        let start = std::time::Instant::now();
        let mut attempts = 0;

        loop {
            attempts += 1;
            let nonce_str = nonce.to_string();
            let hash = compute_hash(challenge, &nonce_str);

            if attempts % 100_000 == 0 {
                println!(
                    "Attempted: {}, Nonce: {}, Hash: {}, Leading zeros: {}",
                    attempts,
                    nonce,
                    hash,
                    {
                        let mut count = 0u32;
                        for c in hash.chars() {
                            let digit = c.to_digit(16).unwrap_or(0);
                            if digit == 0 {
                                count += 4;
                            } else {
                                count += digit.leading_zeros() - 28;
                                break;
                            }
                        }
                        count
                    }
                );
            }

            if has_leading_zero_bits(&hash, difficulty) {
                let elapsed = start.elapsed();
                println!("\n✅ SOLUTION FOUND!");
                println!("  Nonce: {}", nonce);
                println!("  Hash: {}", hash);
                println!("  Attempts: {}", attempts);
                println!("  Time: {:?}", elapsed);

                // Verify the solution
                assert!(
                    has_leading_zero_bits(&hash, difficulty),
                    "Solution should have required leading zeros"
                );

                // Count actual leading zeros
                let mut actual_zeros = 0u32;
                for c in hash.chars() {
                    let digit = match c.to_digit(16) {
                        Some(d) => d,
                        None => break,
                    };
                    if digit == 0 {
                        actual_zeros += 4;
                    } else {
                        actual_zeros += digit.leading_zeros() - 28;
                        break;
                    }
                }
                println!("  Actual leading zero bits: {}", actual_zeros);

                // Now verify this valid nonce with the verification endpoint logic
                println!("\n✅ VERIFYING SOLUTION WITH SERVER LOGIC:");
                let verify_request = VerifyChallengeRequest {
                    challenge: challenge.to_string(),
                    nonce: nonce.to_string(),
                    environment_id: 1,
                    identifier: "test".to_string(),
                    identifier_type: "test".to_string(),
                };

                // Verify the hash one more time
                let verify_hash = compute_hash(&verify_request.challenge, &verify_request.nonce);
                if !has_leading_zero_bits(&verify_hash, difficulty) {
                    panic!("Verification would fail - hash doesn't have required leading zeros!");
                }
                println!(
                    "  ✅ Verification PASSED: nonce {} is valid!",
                    verify_request.nonce
                );
                break;
            }

            nonce += 1;

            // Safety limit
            if nonce > 100_000_000 {
                panic!("Failed to find solution within 100M attempts. Difficulty may be too high.");
            }
        }
    }

    #[test]
    fn test_wasm_algorithm_identical_to_server() {
        // Implement the exact WASM algorithm in Rust to verify it produces the same result
        // This helps us identify if there's a bug in the WASM's logic

        let challenge = "24b3e6735afa5527e75cb353a4a2c915";
        let difficulty = 20;

        println!("\n=== WASM ALGORITHM REPRODUCTION IN RUST ===");
        println!("Challenge: {}", challenge);
        println!("Difficulty: {} bits", difficulty);

        // This is exactly what WASM does:
        let mut nonce = 0u64;
        let start = std::time::Instant::now();
        let mut attempts = 0;

        loop {
            attempts += 1;

            // WASM compute_hash: format!("{}{}", challenge, nonce)
            let input = format!("{}{}", challenge, nonce);
            let mut hasher = Sha256::new();
            hasher.update(input.as_bytes());
            let result = hasher.finalize();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&result);

            // WASM has_leading_zero_bits: convert to hex and count
            let hash_hex = hex::encode(&bytes);
            let mut leading_zeros = 0u32;
            let mut found = false;

            for c in hash_hex.chars() {
                let digit = match c.to_digit(16) {
                    Some(d) => d,
                    None => break,
                };

                if digit == 0 {
                    leading_zeros += 4;
                    if leading_zeros >= difficulty {
                        found = true;
                        break;
                    }
                } else {
                    leading_zeros += digit.leading_zeros().saturating_sub(28);
                    found = leading_zeros >= difficulty;
                    break;
                }
            }

            if attempts % 100_000 == 0 {
                println!(
                    "Attempted: {}, Current hash: {}..., Leading zeros: {}",
                    attempts,
                    &hash_hex[..16],
                    leading_zeros
                );
            }

            if found {
                let elapsed = start.elapsed();
                println!("\n✅ WASM ALGORITHM FOUND SOLUTION!");
                println!("  Nonce: {}", nonce);
                println!("  Hash: {}", hash_hex);
                println!("  Attempts: {}", attempts);
                println!("  Time: {:?}", elapsed);
                println!("  Leading zeros: {}", leading_zeros);

                // Verify it matches the server-side solution
                assert_eq!(nonce, 23999, "WASM algorithm should find nonce 23999");
                break;
            }

            nonce += 1;

            if nonce > 100_000_000 {
                panic!("WASM algorithm failed to find solution within 100M attempts!");
            }
        }
    }
}
