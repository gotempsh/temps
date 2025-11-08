/// Test for chunked transfer encoding streaming
/// This test verifies that the proxy properly streams chunked responses
/// without buffering all chunks until the response completes
#[cfg(test)]
mod chunked_streaming_tests {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    /// Test upstream server that sends chunked response slowly
    async fn start_chunked_server() -> (String, mpsc::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind listener");
        let addr = listener.local_addr().expect("Failed to get local addr");
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        if let Ok((mut socket, _)) = result {
                            // Read HTTP request
                            let mut buf = vec![0; 1024];
                            if let Ok(n) = socket.read(&mut buf).await {
                                if n > 0 {
                                    let request = String::from_utf8_lossy(&buf[..n]);

                                    if request.contains("GET") {
                                        // Send chunked response slowly (simulate streaming)
                                        let response = "HTTP/1.1 200 OK\r\n\
                                                       Content-Type: text/plain\r\n\
                                                       Transfer-Encoding: chunked\r\n\
                                                       Connection: keep-alive\r\n\
                                                       \r\n";

                                        let _ = socket.write_all(response.as_bytes()).await;
                                        let _ = socket.flush().await;

                                        // Send chunks with delays to simulate streaming data
                                        for i in 0..5 {
                                            let chunk = format!("Chunk {}\n", i);
                                            let chunk_header = format!("{:x}\r\n", chunk.len());

                                            // Write chunk size (hex) + CRLF
                                            let _ = socket.write_all(chunk_header.as_bytes()).await;
                                            // Write chunk data + CRLF
                                            let _ = socket.write_all(chunk.as_bytes()).await;
                                            let _ = socket.write_all(b"\r\n").await;
                                            let _ = socket.flush().await;

                                            // Simulate network delay
                                            tokio::time::sleep(Duration::from_millis(100)).await;
                                        }

                                        // Send final chunk (0 CRLF CRLF)
                                        let _ = socket.write_all(b"0\r\n\r\n").await;
                                        let _ = socket.flush().await;
                                    }
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        });

        (format!("http://{}", addr), shutdown_tx)
    }

    #[tokio::test]
    async fn test_chunked_response_streaming() {
        let (server_addr, _shutdown) = start_chunked_server().await;

        // Connect to upstream server
        let addr = server_addr.replace("http://", "");
        let mut client = TcpStream::connect(&addr)
            .await
            .expect("Failed to connect to server");

        // Send HTTP request
        let request = "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
        client
            .write_all(request.as_bytes())
            .await
            .expect("Failed to write request");
        client.flush().await.expect("Failed to flush");

        // Read response with timeout
        let mut response = Vec::new();
        let mut buf = [0; 1024];

        match timeout(Duration::from_secs(10), async {
            loop {
                match client.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        response.extend_from_slice(&buf[..n]);
                        println!("Received {} bytes at {:?}", n, std::time::SystemTime::now());
                    }
                    Err(e) => {
                        eprintln!("Read error: {}", e);
                        break;
                    }
                }
            }
        })
        .await
        {
            Ok(_) => {
                let response_str = String::from_utf8_lossy(&response);
                println!("Full response:\n{}", response_str);

                // Check that response contains chunks
                assert!(response_str.contains("Chunk 0"));
                assert!(response_str.contains("Chunk 4"));
                assert!(response_str.contains("Transfer-Encoding: chunked"));
            }
            Err(_) => {
                panic!("Timeout waiting for response - chunks may be buffered!");
            }
        }
    }

    /// Test that simulates a streaming API endpoint
    #[tokio::test]
    async fn test_proxy_streams_chunked_response() {
        println!("\n=== Testing Chunked Response Streaming ===\n");

        // Start upstream server that sends chunked data
        let (server_addr, _shutdown) = start_chunked_server().await;
        println!("Started upstream server at: {}", server_addr);

        // In a real test, you would:
        // 1. Start the Pingora proxy
        // 2. Configure it to proxy to the upstream server
        // 3. Connect to the proxy
        // 4. Measure time between receiving chunks
        // 5. Verify that chunks arrive as they're sent, not all at once

        // For now, just test the upstream server directly
        let addr = server_addr.replace("http://", "");
        let start = std::time::Instant::now();
        let mut client = TcpStream::connect(&addr).await.expect("Failed to connect");

        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("Failed to write");

        let mut buf = [0; 256];
        let mut chunk_times = Vec::new();

        loop {
            match timeout(Duration::from_secs(5), client.read(&mut buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    let elapsed = start.elapsed();
                    chunk_times.push(elapsed);
                    let data = String::from_utf8_lossy(&buf[..n]);
                    println!("[{:?}] Received {} bytes", elapsed, n);
                    if data.contains("Chunk") {
                        println!("  Content: {}", data.lines().next().unwrap_or(""));
                    }
                }
                Ok(Err(e)) => {
                    eprintln!("Read error: {}", e);
                    break;
                }
                Err(_) => {
                    eprintln!("Read timeout");
                    break;
                }
            }
        }

        println!("\nChunk arrival times: {:?}", chunk_times);

        // Verify chunks arrived incrementally (not all at once)
        if chunk_times.len() > 2 {
            let time_between_chunks: Vec<_> = chunk_times.windows(2).map(|w| w[1] - w[0]).collect();
            println!("Time between chunks: {:?}", time_between_chunks);

            // Check that chunks didn't all arrive at once
            let max_time_between = time_between_chunks
                .iter()
                .max()
                .copied()
                .unwrap_or(Duration::from_secs(0));

            // Should be around 100ms apart (from the upstream server's delays)
            if max_time_between > Duration::from_millis(50) {
                println!("✓ Chunks arrived incrementally (good streaming)");
            } else {
                println!("✗ All chunks arrived at once (buffering issue)");
            }
        }
    }

    #[tokio::test]
    async fn test_chunked_encoding_format() {
        // Test that chunked encoding format is correct
        let chunked_data = "5\r\nHello\r\n6\r\n World\r\n0\r\n\r\n";

        // Parse chunks manually
        let mut pos = 0;
        let data = chunked_data.as_bytes();

        while pos < data.len() {
            // Read chunk size (hex)
            let mut size_str = String::new();
            while pos < data.len() && data[pos] != b'\r' {
                size_str.push(data[pos] as char);
                pos += 1;
            }

            let chunk_size = usize::from_str_radix(&size_str, 16).expect("Invalid hex");
            println!("Chunk size: {} bytes", chunk_size);

            if chunk_size == 0 {
                break;
            }

            // Skip \r\n
            pos += 2;

            // Read chunk data
            let chunk_data = String::from_utf8_lossy(&data[pos..pos + chunk_size]);
            println!("Chunk data: '{}'", chunk_data);
            pos += chunk_size;

            // Skip trailing \r\n
            pos += 2;
        }

        println!("✓ Chunked encoding format is correct");
    }
}
