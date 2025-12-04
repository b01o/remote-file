use core::panic;
use std::{io::Write, net::SocketAddr, path::Path};

use axum::Router;
use remote_file::HttpFile;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tower_http::services::ServeDir;

async fn setup_file_server(path: String, addr: SocketAddr) {
    let app = Router::new().nest_service("/files", ServeDir::new(path));

    // Bind to a socket address
    log::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn create_test_file(file_path: &Path) {
    // create a 20MB random file for test
    let mut file = std::fs::File::options()
        .create(true)
        .write(true)
        .truncate(true)
        .open(file_path)
        .unwrap();

    let mut buf = vec![0u8; 10 * 1024 * 1024];
    for _ in 0..2 {
        rand::fill(&mut buf[..]);
        file.write_all(&buf).unwrap();
    }
    file.sync_all().unwrap();
}

#[tokio::test]
async fn random_file_read() {
    let workdir = std::env::temp_dir();
    let file_name = "random_access_test_file.bin";
    let file_path = workdir.join(file_name);

    // create test file
    create_test_file(&file_path);
    let mut file = tokio::fs::File::open(&file_path).await.unwrap();

    let addr = SocketAddr::from(([0, 0, 0, 0], 13567));
    let url = format!("http://localhost:13567/files/{}", file_name);

    // start file server
    let workdir_str = workdir.to_string_lossy().into_owned();
    tokio::spawn(async move {
        setup_file_server(workdir_str, addr).await;
        panic!("file server exited unexpectedly");
    });

    let client = reqwest::Client::new();
    let resp = client
        .head(&url)
        .send()
        .await
        .expect("failed to send request, forgot to start server?");

    // len should be the same
    let len1 = file.metadata().await.unwrap().len();
    let len2 = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(len1, len2, "content length should be the same");

    // two file content should be the same
    // compare two files chunk by chunk
    let mut resp = client.get(&url).send().await.unwrap();
    while let Some(chunk) = resp.chunk().await.unwrap() {
        let mut buf = vec![0u8; chunk.len()];
        file.read_exact(&mut buf).await.unwrap();
        assert_eq!(
            buf.as_slice(),
            chunk.as_ref(),
            "file content should be the same"
        );
    }

    let mut http_reader = HttpFile::new(client, &url).await.unwrap();
    // generate some random read positions
    let mut positions = vec![];
    for _ in 0..100 {
        let pos = (rand::random::<u64>() % len1).min(len1 - 1024 * 1024);
        positions.push(pos);
    }

    for pos in positions {
        // seek to the position
        http_reader
            .seek(std::io::SeekFrom::Start(pos))
            .await
            .unwrap();
        file.seek(std::io::SeekFrom::Start(pos)).await.unwrap();

        let mut buf1 = vec![0u8; 1024 * 1024];
        let mut buf2 = vec![0u8; 1024 * 1024];
        http_reader.read_exact(&mut buf1).await.unwrap();
        file.read_exact(&mut buf2).await.unwrap();
        assert_eq!(buf1, buf2, "file content should be the same at pos {}", pos);
    }
}

#[tokio::test]
async fn seek_to_end_of_file() {
    let workdir = std::env::temp_dir();
    let file_name = "seek_eof_test_file.bin";
    let file_path = workdir.join(file_name);

    // create test file
    create_test_file(&file_path);
    let mut local_file = tokio::fs::File::open(&file_path).await.unwrap();

    let addr = SocketAddr::from(([0, 0, 0, 0], 13568));
    let url = format!("http://localhost:13568/files/{}", file_name);

    // start file server
    let workdir_str = workdir.to_string_lossy().into_owned();
    tokio::spawn(async move {
        setup_file_server(workdir_str, addr).await;
        panic!("file server exited unexpectedly");
    });

    let client = reqwest::Client::new();
    // Wait for server to be ready
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let mut http_file = HttpFile::new(client, &url).await.unwrap();
    let file_length = http_file.content_length().unwrap();

    // Test 1: Seek to EOF (file_length) should succeed
    let local_pos = local_file
        .seek(std::io::SeekFrom::Start(file_length))
        .await
        .unwrap();
    let remote_pos = http_file
        .seek(std::io::SeekFrom::Start(file_length))
        .await
        .unwrap();
    assert_eq!(local_pos, file_length, "local file should seek to EOF");
    assert_eq!(remote_pos, file_length, "remote file should seek to EOF");
    assert_eq!(
        local_pos, remote_pos,
        "both files should be at the same position"
    );

    // Test 2: Reading at EOF should return 0 bytes
    let mut buf1 = [0u8; 10];
    let mut buf2 = [0u8; 10];
    let local_bytes = local_file.read(&mut buf1).await.unwrap();
    let remote_bytes = http_file.read(&mut buf2).await.unwrap();
    assert_eq!(local_bytes, 0, "local file should read 0 bytes at EOF");
    assert_eq!(remote_bytes, 0, "remote file should read 0 bytes at EOF");
    assert_eq!(
        local_bytes, remote_bytes,
        "both should read the same number of bytes"
    );

    // Test 3: Seek beyond EOF should fail for both
    let _local_result = local_file
        .seek(std::io::SeekFrom::Start(file_length + 1))
        .await;
    let remote_result = http_file
        .seek(std::io::SeekFrom::Start(file_length + 1))
        .await;

    // For local file, seeking beyond EOF might succeed (depends on implementation)
    // For remote file, it should fail
    assert!(
        remote_result.is_err(),
        "remote file should fail when seeking beyond EOF"
    );

    // Test 4: Seek to EOF using SeekFrom::End(0)
    let local_pos = local_file.seek(std::io::SeekFrom::End(0)).await.unwrap();
    let remote_pos = http_file.seek(std::io::SeekFrom::End(0)).await.unwrap();
    assert_eq!(
        local_pos, file_length,
        "local file should be at EOF using End(0)"
    );
    assert_eq!(
        remote_pos, file_length,
        "remote file should be at EOF using End(0)"
    );

    // Test 5: Reading after seeking to end should still return 0 bytes
    let remote_bytes = http_file.read(&mut buf2).await.unwrap();
    assert_eq!(remote_bytes, 0, "should still read 0 bytes at EOF");
}
