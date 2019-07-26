// vim: tw=80
//! A struct with methods that take arguments by mutable reference.

use mockall::*;

mock!{
    Foo {
        fn foo(&self, x: &mut u32);
        fn bar(&self, y: &'static mut u32);
    }
}

#[test]
fn returning() {
    let mut x: u32 = 5;
    let mut mock = MockFoo::new();
    mock.expect_foo()
        .withf(|x| *x == 5)
        .returning(|x| { *x = 42;} );
    mock.foo(&mut x);
    assert_eq!(x, 42);
}
