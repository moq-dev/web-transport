# [M] Accept typed native transport configuration

## Goal

Quinn client and server builders accept caller-provided TransportConfig without duplicating endpoint or TLS construction.

## Plan

Add a composable typed configuration input and retain the current defaults when omitted. Apply configuration across roots, certificate-hash and disabled-verification client construction, and server construction. Keep the existing explicit congestion-controller option as an overlay on the supplied configuration so unrelated options survive; document that precedence.

Test preservation of timeout, keepalive and congestion-control settings through each construction path. Read the existing builder transport regression before extending it. This is a shared native API prerequisite for Node options, not a new cross-platform configuration abstraction.
