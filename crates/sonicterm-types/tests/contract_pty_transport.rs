//! Contract: `PtyTransport` must be object-safe and Send.
//!
//! Methods: read, write, resize, try_wait. Implementers must kill the
//! child on Drop (see LM-007 in landmines.toml).

use sonicterm_types::PtyTransport;

#[test]
fn pty_transport_is_object_safe() {
    // Compile-time witness: this type only exists if `dyn PtyTransport`
    // is constructable, which requires object-safety.
    fn _accept(_: Box<dyn PtyTransport>) {}
}

#[test]
fn pty_transport_is_send() {
    fn assert_send<T: Send + ?Sized>() {}
    assert_send::<dyn PtyTransport>();
}

#[test]
fn pty_transport_method_shapes() {
    // Mock implementer to lock the trait shape in a test. If the trait
    // changes, this won't compile.
    struct Mock;
    impl PtyTransport for Mock {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Ok(0)
        }
        fn resize(&mut self, _cols: u16, _rows: u16) -> std::io::Result<()> {
            Ok(())
        }
        fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
            Ok(None)
        }
    }
    let mut m: Box<dyn PtyTransport> = Box::new(Mock);
    let _ = m.read(&mut [0u8; 4]).unwrap();
    let _ = m.write(b"x").unwrap();
    m.resize(80, 24).unwrap();
    assert_eq!(m.try_wait().unwrap(), None);
}
