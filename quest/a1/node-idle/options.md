# [M] Expose Node idle timeout and keepalive options

## Goal

Node clients and servers can configure idle timeout and keepalive explicitly without changing omitted-option defaults.

## Plan

Expose explicit millisecond options, validate finite representable values, and distinguish omission from a documented disabled state. Do not accidentally interpret an omitted idle timeout as disabling timeout. Use the typed native transport configuration rather than rebuilding TLS or endpoint internals in NAPI.

Keep the default idle timeout and absent keepalive behavior. Test configured expiry, preserved default configuration, disabled behavior, keepalive maintaining an otherwise idle connection and invalid input rejection. Use deterministic configuration/unit coverage where possible; exercise actual Node runtime sessions for the integration behavior. Update generated declarations through the normal generation path, wrapper options and examples in the same PR.

## Required

- [Native transport configuration](/quest/a1/node-idle/native-transport-config.md) - supply options consistently through existing construction paths

## Closes

- [#219](https://github.com/moq-dev/web-transport/issues/219) - the implementation completes this report
