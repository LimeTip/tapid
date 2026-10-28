`download-localhost-cert.pem` and `download-localhost-key.pem` are public test
credentials for an isolated loopback HTTPS server, not release signing material.
The downloader probe accepts only this certificate and the exact loopback origin
inside its validation child process. It never changes certificate stores or
production TLS validation. Cookies and proxies are disabled only in this fixture
so unrelated host configuration does not affect its transport assertions.

The self-signed certificate expires in 2036. Regenerate both files together with
OpenSSL when needed (the private key is intentionally committed):

```sh
openssl req -x509 -newkey rsa:2048 -nodes -keyout tools/release/fixtures/download-localhost-key.pem -out tools/release/fixtures/download-localhost-cert.pem -days 3650 -subj '/CN=localhost' -addext 'subjectAltName=DNS:localhost,IP:127.0.0.1'
```
