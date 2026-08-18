## MODIFIED Requirements

### Requirement: Core documentation covers the product journey

The site SHALL document, without requiring a source checkout: the product overview, a working quick start, installation of the router and the node-agent, `fleet.yaml` inventory (including the rule that YAML overlays are tunables-only and top-level `nodes:` is a hard config error), optional host discovery (CIDR scan, Tailscale peer enumeration, LAN agent heartbeat; `fleet.yaml` as an optional pin; discovery never writes the file), the node-agent setup/serve model, and the Verda + RunPod cloud providers.

#### Scenario: First-time visitor reaches a working quick start

- **WHEN** a first-time visitor starts at the homepage and follows the navigation
- **THEN** they can reach a quick-start page whose commands match the shipped product (router, node-agent, fleet file or discovery CIDRs), and a fleet guide that names the tunables-only constraint and optional discovery

#### Scenario: Cloud guide names only supported providers

- **WHEN** a reader opens the cloud autoscaling guide
- **THEN** it covers Verda and RunPod and describes tunnel/loopback-only cloud URLs with `public_url_blocked` for public endpoints, and mentions no other provider

#### Scenario: Fleet guide explains discovery pins

- **WHEN** a reader opens the fleet inventory guide
- **THEN** it states that `fleet.yaml` URLs are optional when discovery is enabled, that scan adopts node-agent hosts only, and that enroll/discovery never write `fleet.yaml`
