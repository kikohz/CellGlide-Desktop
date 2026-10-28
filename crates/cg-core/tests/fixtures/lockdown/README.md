These RSA certificates and keys exist only for the fake lockdownd in cg-core's tests. They have nothing to do with any real iPhone or computer pairing.

- `host-cert.pem` / `host-key.pem` stand in for a pairing record's HostCertificate and HostPrivateKey. The key is PKCS#1 (`BEGIN RSA PRIVATE KEY`), as usbmuxd stores it. The certificate is marked `CA:FALSE`, like a real host certificate, so the fake can verify it as a client certificate.
- `device-cert.pem` / `device-key.pem` are the fake phone's TLS server identity. The key is PKCS#8.

Generated once with:

    openssl req -x509 -newkey rsa:2048 -nodes -days 36500 -subj "/CN=CellGlide test host" -addext "basicConstraints=critical,CA:FALSE" -keyout host-key.pem -out host-cert.pem
    openssl rsa -in host-key.pem -traditional -out host-key.pem
    openssl req -x509 -newkey rsa:2048 -nodes -days 36500 -subj "/CN=CellGlide test device" -keyout device-key.pem -out device-cert.pem
