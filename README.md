[![crates.io](https://img.shields.io/crates/v/remote-file)](https://crates.io/crates/remote-file)
[![docs.rs](https://img.shields.io/docsrs/remote-file)](https://docs.rs/remote-file)

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

### Notes
* The `HttpFile` itself will try to make as few network requests as possible, i.e., it will not make a new request if the seek position is the same as the current position.
* Keep in mind that seeking in a remote file is not as efficient as seeking in a local file, as it requires additional network requests, which brings orders of magnitude more latency. If you need to perform small seeks frequently, consider reading a larger chunk of data into memory and seeking within that buffer instead.
* It does not implement caching, if you need caching, consider wrapping it to a new type and implementing your own caching logic.
* It does not implement `AsyncWrite`, as writing to a remote file over HTTP is not supported.

### Plans
* Supports more protocols, e.g., FTP, S3, etc.
* Implement caching layer.
* More robust retry mechanism.

#### License
<sup>
This project is licensed under the MIT License.
See the <a href="LICENSE">LICENSE</a> file for details.
</sup>




