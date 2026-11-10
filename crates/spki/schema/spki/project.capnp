@0xdaa3b11b4bcfdc42;
# Authoring metadata only. Not a Sandstorm runtime-manifest extension.
struct Project {
  formatVersion @0 :UInt16;
  worker @1 :Worker;
  compatibilityDate @2 :Text;
  compatibilityFlags @3 :List(Text);
  runtime @4 :Runtime;
  compiler @5 :Compiler;
  preview @6 :Preview;
  # Root schemas; transitive local imports are compiled for both worker and browser.
  schemas @9 :List(Text);
  # Browser ES modules and extracted CSS are served below /assets/.
  ui @7 :Ui;
  # Public directories merged at /. Matching files take precedence over worker fetch.
  # Hidden files, symlinks, duplicate/reserved URLs and files over 1 MiB are rejected.
  assets @8 :List(Text);
  # Custom env bindings. Data files are project-relative and included in the artifact.
  # Runtime storage/RPC names are reserved. Service currently supports only "main".
  bindings @10 :List(Binding);
  struct Binding {
    name @0 :Text;
    union {
      text @1 :Text;
      json @2 :Text;
      dataFile @3 :Text;
      service @4 :Text;
    }
  }
  struct Ui { entrypoints @0 :List(Text); }
  struct Worker { entrypoint @0 :Text; }
  struct Runtime { release @0 :Text; hostSha256 @1 :Text; }
  struct Compiler { version @0 :Text; executable @1 :Text = "esbuild"; }
  struct Preview { profile @0 :Text = "default"; }
}
