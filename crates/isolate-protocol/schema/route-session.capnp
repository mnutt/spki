@0x8fb1b6e49fa977d2;
using Grain = import "sandstorm/grain.capnp";
using Api = import "sandstorm/api-session.capnp";
# Internal composite interface; callers use the standard parent interfaces.
interface RouteSession extends(Api.ApiSession, Grain.AppPersistent) {}
struct RouteObjectId {
  format @0 :Text;
  nativeInterface @1 :Text;
  pathPrefix @2 :Text;
}
