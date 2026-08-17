# router-console Specification

## Purpose
Serves the embedded router console UI over HTTP: static console assets reachable without the admin bearer, while every piece of fleet data flows through the authenticated admin API.

## Requirements

### Requirement: Console assets are served without admin auth

`GET /router/ui` and `GET /router/ui/{path}` SHALL serve the embedded console without requiring the admin bearer, with the index document as the fallback for client-side routes. Served asset types SHALL be limited to the embedded static set (html, javascript, css); the console surface SHALL NOT expose fleet data directly, and requesting a non-static console path SHALL still resolve to the console (not to a proxied or API response).

#### Scenario: Console opens without a token

- **WHEN** a browser requests `GET /router/ui` with no authorization header
- **THEN** the console index document is served with a 200

#### Scenario: Client-side route falls back to the index

- **WHEN** a browser requests `GET /router/ui/jobs` directly (a client-side route with no matching asset)
- **THEN** the console index document is served

### Requirement: Console data requires the admin API

The console SHALL obtain all fleet, node, model, and job data exclusively from authenticated `/router/v1/*` endpoints. Without a valid admin bearer, console data requests SHALL fail closed with 403 and the console MUST NOT fall back to unauthenticated data sources.

#### Scenario: Console data requests fail closed

- **WHEN** a console page calls an admin endpoint without a valid bearer token
- **THEN** the admin API returns 403 and no fleet, node, model, or job data is returned
