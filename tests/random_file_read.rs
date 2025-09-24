use core::panic;
use std::{io::Write, net::SocketAddr, path::Path};

use axum::Router;
use remote_file::HttpFile;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tower_http::services::ServeDir;

async fn setup_file_server(path: String, addr: SocketAddr) {
    let app = Router::new()
        .nest_service("/files", ServeDir::new(path));
    
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
        .open(&file_path)
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
    let resp = client.head(&url).send().await.expect("failed to send request, forgot to start server?");

    // len should be the same
    let len1 = file.metadata().await.unwrap().len();
    let len2 = resp.headers()
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
        assert_eq!(buf.as_slice(), chunk.as_ref(), "file content should be the same");
    }

    let mut http_reader = HttpFile::new(client, &url).await.unwrap();
    // generate some random read positions
    let mut positions = vec![];
    for _ in 0..100 {
        let pos = (rand::random::<u64>() % len1).min(len1 - 1024*1024);
        positions.push(pos);
    }

    for pos in positions {
        // seek to the position
        http_reader.seek(std::io::SeekFrom::Start(pos)).await.unwrap();
        file.seek(std::io::SeekFrom::Start(pos)).await.unwrap();

        let mut buf1 = vec![0u8; 1024*1024];
        let mut buf2 = vec![0u8; 1024*1024];
        http_reader.read_exact(&mut buf1).await.unwrap();
        file.read_exact(&mut buf2).await.unwrap();
        assert_eq!(buf1, buf2, "file content should be the same at pos {}", pos);
    }

}