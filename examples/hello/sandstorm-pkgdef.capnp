@0x89ba8310704d96c3;
using Spk = import "/sandstorm/package.capnp";
using Spki = import "/spki/project.capnp";
# From the repository root, after building spki and fetching esbuild:
#   target/debug/spki --project examples/hello auth
#   target/debug/spki --project examples/hello dev --esbuild "$PWD/build/tools/esbuild"
# Uses its own local credential profile; no credentials are stored in this example.
# Authoring template. Set id from spki keygen before packaging; native commands are generated.
const pkgdef :Spk.PackageDefinition = (
  manifest = (
    appTitle = (defaultText = "Hello"), appVersion = 1,
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
  compiler = (version = "0.25.12", executable = "esbuild"),
  preview = (profile = "hello")
);
