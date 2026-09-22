# Compatibility and versioning

## `extension_api_version`

Integer **major** of the Extension API. Independent from:

- Impetus application semver
- Package `version`
- Manifest `schema_version`

Hosts advertise `SupportedApiRange` (`CURRENT_SUPPORTED_RANGE` in the SDK).
Packages outside the range are rejected with `CompatError` without affecting
other extensions.

## Policy for core releases

Bump Impetus app version freely. Only bump `EXTENSION_API_VERSION` / supported
range when the public SDK contract changes in a breaking way. Prefer additive
changes inside the same major.
