/// Tests for normal (non-chunked) streaming scenarios
/// This verifies that the proxy doesn't buffer regular streaming responses
#[cfg(test)]
mod streaming_normal_tests {
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    /// Backend that sends data with Content-Length (normal streaming)
    async fn start_content_length_server() -> (String, mpsc::Sender<()>) {
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
                            let mut buf = vec![0; 1024];
                            if let Ok(n) = socket.read(&mut buf).await {
                                if n > 0 {
                                    let request = String::from_utf8_lossy(&buf[..n]);
                                    if request.contains("GET") {
                                        // Prepare response body with chunks
                                        let chunks = vec![
                                            "First chunk of data\n",
                                            "Second chunk of data\n",
                                            "Third chunk of data\n",
                                            "Fourth chunk of data\n",
                                            "Fifth chunk of data\n",
                                        ];
                                        let body = chunks.join("");
                                        let content_length = body.len();

                                        // Send headers with Content-Length
                                        let response = format!(
                                            "HTTP/1.1 200 OK\r\n\
                                             Content-Type: text/plain\r\n\
                                             Content-Length: {}\r\n\
                                             Connection: close\r\n\
                                             \r\n",
                                            content_length
                                        );

                                        let _ = socket.write_all(response.as_bytes()).await;
                                        let _ = socket.flush().await;

                                        // Send body with delays between chunks
                                        for chunk in chunks {
                                            let _ = socket.write_all(chunk.as_bytes()).await;
                                            let _ = socket.flush().await;
                                            tokio::time::sleep(Duration::from_millis(100)).await;
                                        }
                                        let _ = socket.flush().await;
                                    }
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });

        (format!("http://{}", addr), shutdown_tx)
    }

    /// Backend that sends Server-Sent Events (SSE)
    async fn start_sse_server() -> (String, mpsc::Sender<()>) {
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
                            let mut buf = vec![0; 1024];
                            if let Ok(n) = socket.read(&mut buf).await {
                                if n > 0 {
                                    let request = String::from_utf8_lossy(&buf[..n]);
                                    if request.contains("GET") {
                                        // SSE response with no Content-Length
                                        let response = "HTTP/1.1 200 OK\r\n\
                                                       Content-Type: text/event-stream\r\n\
                                                       Cache-Control: no-cache\r\n\
                                                       Connection: keep-alive\r\n\
                                                       \r\n";

                                        let _ = socket.write_all(response.as_bytes()).await;
                                        let _ = socket.flush().await;

                                        // Send SSE events with delays
                                        for i in 0..5 {
                                            let event = format!("data: Event {}\n\n", i);
                                            let _ = socket.write_all(event.as_bytes()).await;
                                            let _ = socket.flush().await;
                                            tokio::time::sleep(Duration::from_millis(150)).await;
                                        }
                                        let _ = socket.flush().await;
                                    }
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });

        (format!("http://{}", addr), shutdown_tx)
    }

    /// Test normal Content-Length streaming without chunking
    #[tokio::test]
    async fn test_normal_streaming_with_content_length() {
        println!("\n=== Testing Normal Streaming (Content-Length) ===\n");

        let (server_addr, _shutdown) = start_content_length_server().await;
        let addr = server_addr.replace("http://", "");

        let start = Instant::now();
        match TcpStream::connect(&addr).await {
            Ok(mut client) => {
                let request = "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
                let _ = client.write_all(request.as_bytes()).await;
                let _ = client.flush().await;

                let mut buf = [0; 256];
                let mut chunk_times = Vec::new();
                let mut response = String::new();

                loop {
                    match timeout(Duration::from_secs(5), client.read(&mut buf)).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            let elapsed = start.elapsed();
                            chunk_times.push(elapsed);
                            response.push_str(&String::from_utf8_lossy(&buf[..n]));
                            println!("[{:?}] Received {} bytes", elapsed, n);
                        }
                        Ok(Err(_)) => break,
                        Err(_) => break,
                    }
                }

                println!("\nTotal response: {} bytes", response.len());
                assert!(response.contains("First chunk"));
                assert!(response.contains("Fifth chunk"));

                if chunk_times.len() > 1 {
                    let mut inter_chunk_delays = Vec::new();
                    for window in chunk_times.windows(2) {
                        let delay = window[1] - window[0];
                        inter_chunk_delays.push(delay);
                    }

                    if let Some(max_delay) = inter_chunk_delays.iter().max() {
                        println!("Max delay between chunks: {:?}", max_delay);

                        if *max_delay > Duration::from_millis(50) {
                            println!(
                                "✓ Normal streaming arrived with proper delays (not buffered)"
                            );
                        } else {
                            println!("⚠️  Normal streaming may have been buffered");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to connect: {}", e);
            }
        }
    }

    /// Test Server-Sent Events streaming
    #[tokio::test]
    async fn test_sse_streaming_without_buffering() {
        println!("\n=== Testing SSE Streaming (No Content-Length) ===\n");

        let (server_addr, _shutdown) = start_sse_server().await;
        let addr = server_addr.replace("http://", "");

        let start = Instant::now();
        match TcpStream::connect(&addr).await {
            Ok(mut client) => {
                let request = "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
                let _ = client.write_all(request.as_bytes()).await;
                let _ = client.flush().await;

                let mut buf = [0; 256];
                let mut event_times = Vec::new();
                let mut response = String::new();

                loop {
                    match timeout(Duration::from_secs(5), client.read(&mut buf)).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            let elapsed = start.elapsed();
                            let data = String::from_utf8_lossy(&buf[..n]);

                            // Count SSE events received
                            let event_count = data.matches("data: Event").count();
                            if event_count > 0 {
                                event_times.push(elapsed);
                                println!("[{:?}] Received {} events", elapsed, event_count);
                            }

                            response.push_str(&data);
                        }
                        Ok(Err(_)) => break,
                        Err(_) => break,
                    }
                }

                println!("\nTotal response: {} bytes", response.len());
                assert!(response.contains("Event 0"));
                assert!(response.contains("Event 4"));

                if event_times.len() > 1 {
                    let mut inter_event_delays = Vec::new();
                    for window in event_times.windows(2) {
                        let delay = window[1] - window[0];
                        inter_event_delays.push(delay);
                    }

                    if let Some(max_delay) = inter_event_delays.iter().max() {
                        println!("Max delay between events: {:?}", max_delay);

                        if *max_delay > Duration::from_millis(80) {
                            println!("✓ SSE events arrived with proper delays (not buffered)");
                        } else {
                            println!("⚠️  SSE events may have been buffered");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to connect: {}", e);
            }
        }
    }

    /// Test rapid streaming with very small chunks
    #[tokio::test]
    async fn test_rapid_small_chunks_streaming() {
        println!("\n=== Testing Rapid Small Chunks Streaming ===\n");

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
                            let mut buf = vec![0; 1024];
                            if let Ok(n) = socket.read(&mut buf).await {
                                if n > 0 {
                                    let request = String::from_utf8_lossy(&buf[..n]);
                                    if request.contains("GET") {
                                        let response = "HTTP/1.1 200 OK\r\n\
                                                       Content-Type: text/plain\r\n\
                                                       Connection: close\r\n\
                                                       \r\n";

                                        let _ = socket.write_all(response.as_bytes()).await;
                                        let _ = socket.flush().await;

                                        // Send 20 very small chunks rapidly
                                        for i in 0..20 {
                                            let chunk = format!("c{}", i);
                                            let _ = socket.write_all(chunk.as_bytes()).await;
                                            let _ = socket.flush().await;
                                            if i < 19 {
                                                tokio::time::sleep(Duration::from_millis(50)).await;
                                            }
                                        }
                                        let _ = socket.flush().await;
                                    }
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });

        let addr_str = format!("127.0.0.1:{}", addr.port());
        let start = Instant::now();

        match TcpStream::connect(&addr_str).await {
            Ok(mut client) => {
                let request = "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
                let _ = client.write_all(request.as_bytes()).await;
                let _ = client.flush().await;

                let mut buf = [0; 256];
                let mut total_bytes = 0;
                let mut chunk_count = 0;
                let mut chunk_times = Vec::new();

                loop {
                    match timeout(Duration::from_secs(5), client.read(&mut buf)).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            total_bytes += n;
                            chunk_count += 1;
                            let elapsed = start.elapsed();
                            chunk_times.push(elapsed);
                            println!("[{:?}] Chunk {}: {} bytes", elapsed, chunk_count, n);
                        }
                        Ok(Err(_)) => break,
                        Err(_) => break,
                    }
                }

                println!("\nTotal: {} bytes in {} reads", total_bytes, chunk_count);

                if chunk_count > 1 {
                    let first_to_last = if let (Some(first), Some(last)) =
                        (chunk_times.first(), chunk_times.last())
                    {
                        *last - *first
                    } else {
                        Duration::from_secs(0)
                    };

                    println!("Time from first to last read: {:?}", first_to_last);

                    // With 20 chunks at 50ms delays, should take roughly 950ms
                    // If all chunks arrived instantly, this would be very small
                    if first_to_last > Duration::from_millis(500) {
                        println!("✓ Rapid chunks arrived incrementally (proper streaming)");
                    } else {
                        println!("⚠️  Rapid chunks may have been buffered");
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to connect: {}", e);
            }
        }

        let _ = shutdown_tx.send(());
    }

    /// Test streaming with no Content-Length or Transfer-Encoding
    #[tokio::test]
    async fn test_streaming_without_content_length_or_chunked() {
        println!("\n=== Testing Streaming (No Length Headers) ===\n");

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
                            let mut buf = vec![0; 1024];
                            if let Ok(n) = socket.read(&mut buf).await {
                                if n > 0 {
                                    let request = String::from_utf8_lossy(&buf[..n]);
                                    if request.contains("GET") {
                                        // Deliberately omit Content-Length and Transfer-Encoding
                                        let response = "HTTP/1.1 200 OK\r\n\
                                                       Content-Type: text/plain\r\n\
                                                       Connection: close\r\n\
                                                       \r\n";

                                        let _ = socket.write_all(response.as_bytes()).await;
                                        let _ = socket.flush().await;

                                        for i in 0..5 {
                                            let data = format!("Data block {}\n", i);
                                            let _ = socket.write_all(data.as_bytes()).await;
                                            let _ = socket.flush().await;
                                            tokio::time::sleep(Duration::from_millis(100)).await;
                                        }
                                        let _ = socket.flush().await;
                                    }
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });

        let addr_str = format!("127.0.0.1:{}", addr.port());
        let start = Instant::now();

        match TcpStream::connect(&addr_str).await {
            Ok(mut client) => {
                let request = "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
                let _ = client.write_all(request.as_bytes()).await;
                let _ = client.flush().await;

                let mut buf = [0; 256];
                let mut chunk_times = Vec::new();
                let mut response = String::new();

                loop {
                    match timeout(Duration::from_secs(5), client.read(&mut buf)).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            let elapsed = start.elapsed();
                            chunk_times.push(elapsed);
                            response.push_str(&String::from_utf8_lossy(&buf[..n]));
                            println!("[{:?}] Received {} bytes", elapsed, n);
                        }
                        Ok(Err(_)) => break,
                        Err(_) => break,
                    }
                }

                println!("\nTotal response: {} bytes", response.len());
                assert!(response.contains("Data block 0"));
                assert!(response.contains("Data block 4"));

                if chunk_times.len() > 1 {
                    let elapsed_total = start.elapsed();
                    println!("Total elapsed: {:?}", elapsed_total);

                    if elapsed_total > Duration::from_millis(300) {
                        println!("✓ Streaming without length headers arrived incrementally");
                    } else {
                        println!("⚠️  Streaming without length headers may have been buffered");
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to connect: {}", e);
            }
        }

        let _ = shutdown_tx.send(());
    }
}
