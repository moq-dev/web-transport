# [M] Compose a configured native client with the generic API

## Goal

Applications can configure a Quinn client using its native API and convert it into the platform router client while retaining subprotocol configuration.

## Plan

Add a native-only conversion from `web_transport_quinn::Client`, matching existing Server/Session composition. Provide a composable way to configure offered subprotocols on the converted client. Keep construction platform-specific and connection/application logic shared.

Do not add a pretend browser certificate-verification bypass, a browser panic or a new common unsupported-operation path. Document a conditional native setup example and the existing browser trust options. Test native configuration preservation and protocol negotiation, and compile the WASM surface to ensure platform separation remains intact.

## Closes

- [#135](https://github.com/moq-dev/web-transport/issues/135) - the implementation completes this report
