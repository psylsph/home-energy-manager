# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability, please report it privately by opening a [GitHub Security Advisory](https://github.com/psylsph/givenergy-local/security/advisories/new).

Do not file a public issue for security vulnerabilities.

## Scope

This project communicates with inverters on your local network only. It does not connect to any cloud services or expose data externally. The embedded HTTP server binds to `0.0.0.0:7337` — ensure your network is appropriately firewalled.
