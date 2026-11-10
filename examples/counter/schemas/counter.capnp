@0x80246bc0cb264c6a;

interface Counter {
  read @0 () -> (value :Int32, greeting :Text);
  increment @1 () -> (value :Int32);
  reset @2 () -> (value :Int32);
  subscribe @3 (observer :CounterObserver) -> ();
}

interface CounterObserver {
  changed @0 (value :Int32) -> ();
}
