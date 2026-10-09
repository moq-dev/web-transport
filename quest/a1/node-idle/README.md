# Configurable native and Node idle behavior

## Goal

Node idle behavior is configurable through the existing native transport layer with documented defaults.

## Plan

The native configuration and Node option quests are independently reviewable. The epic owns checking the final example against the packaged Node API and confirming the public defaults agree across generated declarations and docs.

## Required

- [Native transport configuration](/quest/a1/node-idle/native-transport-config.md) - typed reusable builder input
- [Node options](/quest/a1/node-idle/options.md) - explicit millisecond options and runtime behavior
