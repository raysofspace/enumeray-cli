# Executable public HTTP contract

- [Execute standalone artifact, collection and saved-body run workflows](HTTP_WORKFLOWS.md).
- [Reproduce exact collection identity bytes](COLLECTION_IDENTITY.md) using
  `collection-identity.py` and the downloadable `digest-vectors/` directory.
- [Migrate candidate CLI output and generated SDK methods](COMPATIBILITY.md).

`scenarios.json` contains complete, inert request/response specimens grouped by
one coherent scenario. The scalar run's original queued acceptance receipt and
later inspections retain the same run, model and publication identities. Queued
and terminal inspections have `phase: null`; only the running inspection has a
phase. Original-receipt replay does not report current execution state.

Every invitation specifies exactly one recipient. The enrollment tokens and
verifier have valid wire shapes but are nonsecret fixture values, not usable
credentials. Token inventory never includes a bearer secret. Scientific-schema
violations appear only on an authorized validation failure, never on an
authentication error. A removed member has `co_admin: false`.

`fragments` are explicitly labeled **complete field values**, with their
containing response and field named in `parent`; they are not complete HTTP
responses. Publication trial evidence is bounded administrative history.
Connector-observed installation digests do not establish scientific validity.

The service exports these exact specimens as named OpenAPI media examples and
validates their schemas, formats and relationships. The website publishes the
same file and examples. Runtime transaction tests exercise independent clients
and the accompanying raw HTTP recipe against a disposable service. Replace
fixture identities with the account, model, publication and connector values
actually approved for your service before submitting work.
