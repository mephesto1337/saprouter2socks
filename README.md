# saprouter2socks

Forward socks5 connect requests to an upstream SAP Router instance

## TL;DR

```bash
cargo run --release -- SAPROUTER:PORT
```

It will open a socks5 server on port 1080 by default and forward all connect requests to the Sap
Router instance with a `NI_ROUTE` request.

