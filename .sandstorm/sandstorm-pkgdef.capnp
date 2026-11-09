@0xe7278c5f807f4df1;
using Spk = import "/sandstorm/package.capnp";
# Development runtime spike; the signing key is in ignored build/preview-keyring.
const pkgdef :Spk.PackageDefinition = (
  id = "vgsmnudpv4aup9cejg27p8cg029av8k3nr7h5yuduycjcz6hn49h",
  manifest = (
    appTitle = (defaultText = "Isolate Preview — Runtime Spike"),
    appVersion = 1,
    appMarketingVersion = (defaultText = "0.1.1-dev"),
    actions = [(input = (none = void), nounPhrase = (defaultText = "preview"), command = .command)],
    continueCommand = .command
  ),
  sourceMap = (searchPath = [(sourcePath = "../build/grain-root")]),
  alwaysInclude = ["."]
);
const command :Spk.Manifest.Command = (
  argv = ["/bin/isolate-preview", "grain", "/bin/isolate-host", "/app/worker.js"]
);
