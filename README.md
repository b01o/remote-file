
## A file implementation for remote assets

This library provides a way to visit a file over HTTP, mimicking the behavior of the standard library's `File` type.

### Highlights
* Supports `AsyncRead` and `AsyncSeek` traits from `tokio`.
* Uses HTTP Range requests to fetch data.
* `stream_position()` is cheap, as it is tracked locally.
* Exposes reqwest's Error through `std::io::Error::Other`.
* Handles transient network errors with retries(currently is a simple retry of 3 attempts).


### Example

```rust no_run
use remote_file::HttpFile;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use reqwest::Client;
#[tokio::main]
async fn main() {
    let url = "http://example.com/largefile";
    let client = Client::new();
    let mut file = HttpFile::new(client, url).await.unwrap();

    // Print the content length
    dbg!(file.content_length());

    // Seek to current position will not make a new network request
    file.seek(std::io::SeekFrom::Start(0)).await.unwrap();
    file.seek(std::io::SeekFrom::Current(0)).await.unwrap();

    let mut buffer = vec![0; 512];

    // Note: succeeding reads will not make a new network request
    // Read 512 bytes
    file.read_exact(&mut buffer).await.unwrap();
    // Read another 512 bytes
    file.read_exact(&mut buffer).await.unwrap();
    
    let pos = file.stream_position().await.unwrap();
    assert_eq!(pos, 1024);
}
```

