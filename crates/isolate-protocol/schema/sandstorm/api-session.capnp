@0xeb014c0c3413cbfb;
# Wire-compatible ApiSession projection, including its public Powerbox tag.
# All methods are inherited from WebSession; remote-IP session parameters are unused.
using WebSession = import "web-session.capnp";
interface ApiSession @0xc879e379c625cdc7 extends(WebSession.WebSession) {
  struct PowerboxTag { canonicalUrl @0 :Text; oauthScopes @1 :List(Text); }
}
