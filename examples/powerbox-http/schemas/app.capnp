@0x9cf51bb90e1c5ed3;
using Http = import "/sandstorm/api-session.capnp";
interface App {
  read @0 (api :Http.ApiSession) -> (text :Text);
}
