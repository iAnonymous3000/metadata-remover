# Security Policy

## Supported Versions

Security fixes are made on `main`. The hosted GitHub Pages build is published from `main` after CI passes.

## Reporting a Vulnerability

Please do not include exploit details or private sample files in a public issue.

Use GitHub private vulnerability reporting if it is available for this repository. If private reporting is not available, open a brief public issue that says you have a security report and include only the affected area and impact summary.

## Scope

Metadata Remover processes untrusted files locally in the browser. High-value reports include parser crashes, file corruption, metadata that survives cleaning unexpectedly, cross-site scripting, service-worker cache issues, or privacy regressions that could cause file data to leave the device.
