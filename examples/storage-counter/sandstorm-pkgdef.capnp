@0xc36c7cc9a1dabade;
using Spk = import "/sandstorm/package.capnp";
using Spki = import "/spki/project.capnp";
# From the repository root, after building spki:
#   target/debug/spki --project examples/storage-counter auth
#   target/debug/spki --project examples/storage-counter dev
# Uses the shared default workspace profile; no credentials are stored in this example.
# Authoring template. Set id from spki keygen before packaging; native commands are generated.
const pkgdef :Spk.PackageDefinition = (
  manifest = (
    appTitle = (defaultText = "Storage Counter"), appVersion = 1,
    appMarketingVersion = (defaultText = "0.1.0"),
    actions = [(input = (none = void), nounPhrase = (defaultText = "document"))]
  )
);
const spki :Spki.Project = (
  formatVersion = 1,
  worker = (entrypoint = "worker.js"),
  schemas = [],
  compatibilityDate = "2026-06-10", compatibilityFlags = [],
  runtime = (release = "0.1.2", hostSha256 = "94b04f0ed5ad0f3ff862dcadd1b7edfe6cad1717587a60b7d8e6775541b538b1"),
  compiler = (version = "1.3.1", name = "rolldown"),
  preview = (profile = "default", project = "storage-counter")
);
