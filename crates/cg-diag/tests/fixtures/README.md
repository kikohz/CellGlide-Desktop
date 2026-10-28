This certificate and key exist only for the fake HTTPS server in cg-diag's upload tests. They have nothing to do with diag.cellglide.com.

Generated once with:

    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 36500 -subj "/CN=localhost" -addext "basicConstraints=critical,CA:FALSE" -addext "subjectAltName=DNS:localhost" -keyout server-key.pem -out server-cert.pem
