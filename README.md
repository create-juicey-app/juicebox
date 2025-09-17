# Juicebox

Fast Catbox-like hotlink file share.

Juicebox is a lightweight, high-speed file hosting and sharing service inspired by [Catbox](https://catbox.moe/). It allows users to quickly upload and share files with direct hotlinking support, making it ideal for sharing images, videos, documents, and other files.

---

## Features

- 🚀 **Fast Uploads**: Optimized for speed and performance.
- 🔗 **Direct Hotlinking**: Instantly share files with direct links.
- 🗂️ **Multiple File Types**: Supports images, videos, documents, and more.
- 🔒 **Privacy-Oriented**: Minimal tracking, no analytics by default.
- 🖥️ **Simple Web UI**: Everything is served and managed by the backend.
- ⚡ **API Support**: Easily integrate file upload and sharing via API.

---

## Getting Started

### Prerequisites

- **Rust** (for building and running Juicebox)

### Installation

#### Clone the repository

```bash
git clone https://github.com/create-juicey-app/juicebox.git
cd juicebox
```

#### Build and Run

```bash
cargo build --release
cargo run --release
```

By default, Juicebox will start its backend server, which serves both the frontend web UI and the API.

---

## Usage

- Open your browser and navigate to the address shown in the terminal (default: [http://localhost:8080](http://localhost:8080)).
- Use the web interface to upload files.
- Share the provided direct link to your uploaded file.

---

## API

You can upload files via a simple POST request:

```http
POST /api/upload
Content-Type: multipart/form-data

file=<your file>
```

**Example (using curl):**

```bash
curl -F 'file=@path/to/yourfile.png' http://localhost:8080/api/upload
```

---

## Contributing

Pull requests and issues are welcome! Please open an issue first to discuss major changes.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/your-feature`)
3. Commit your changes (`git commit -am 'Add new feature'`)
4. Push to the branch (`git push origin feature/your-feature`)
5. Open a pull request

---

## License

MIT License

---

## Acknowledgements

- [Catbox](https://catbox.moe/) for inspiration
- Rust, JavaScript, HTML, CSS communities

---

## Links

- [GitHub Repository](https://github.com/create-juicey-app/juicebox)
- [Issues](https://github.com/create-juicey-app/juicebox/issues)