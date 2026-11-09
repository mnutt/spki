@0x8c87890e4b594db3;
# Test client only: wire-compatible projection of existing Supervisor.getMainView.
# Never used by the application or exposed to workers. No isolate extensions.
using Grain = import "sandstorm/grain.capnp";
interface Supervisor @0x9951c12163385530 {
  getMainView @0 () -> (view :Grain.UiView);
}
