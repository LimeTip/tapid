//! Disposable running local-server class; no persistent registry registration.
use std::{
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};
use windows_sys::{Win32::System::Com::*, core::GUID};

const UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const CLASS_FACTORY: GUID = GUID::from_u128(0x00000001_0000_0000_c000_000000000046);

#[repr(C)]
struct Vtable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    create:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const GUID, *mut *mut c_void) -> i32,
    lock: unsafe extern "system" fn(*mut c_void, i32) -> i32,
}

#[repr(C)]
struct UnknownVtable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct Factory {
    vtable: *const Vtable,
    references: AtomicU32,
    creations: Arc<AtomicU32>,
}

fn same(a: GUID, b: GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

// COM supplies live interface pointers and valid IID/output pointers. The MTA
// may call concurrently; only atomic state is shared. No callback unwinds.
unsafe extern "system" fn query(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return 0x80004003_u32 as i32; // E_POINTER
    }
    unsafe {
        *output = std::ptr::null_mut();
        if iid.is_null() {
            return 0x80004003_u32 as i32;
        }
        if !same(*iid, UNKNOWN) && !same(*iid, CLASS_FACTORY) {
            return 0x80004002_u32 as i32;
        }
        add_ref(this);
        *output = this;
    }
    0
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    unsafe {
        (*this.cast::<Factory>())
            .references
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let factory = this.cast::<Factory>();
    let remaining = unsafe { (*factory).references.fetch_sub(1, Ordering::AcqRel) - 1 };
    if remaining == 0 {
        unsafe { drop(Box::from_raw(factory)) };
    }
    remaining
}

unsafe extern "system" fn create(
    this: *mut c_void,
    outer: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return 0x80004003_u32 as i32;
    }
    unsafe { *output = std::ptr::null_mut() };
    if !outer.is_null() {
        return 0x80040110_u32 as i32; // CLASS_E_NOAGGREGATION
    }
    // The inert object exposes only the same fixture's IUnknown/IClassFactory;
    // it performs no filesystem, process, shell or network operations.
    let result = unsafe { query(this, iid, output) };
    if result == 0 {
        unsafe {
            (*this.cast::<Factory>())
                .creations
                .fetch_add(1, Ordering::SeqCst)
        };
    }
    result
}

unsafe extern "system" fn lock(this: *mut c_void, enabled: i32) -> i32 {
    // COM implicitly locks a marshaled IClassFactory until the client releases
    // its proxy. Each balanced lock owns a reference, independently of the
    // registration and the test's owner reference.
    unsafe {
        if enabled != 0 {
            add_ref(this);
        } else {
            release(this);
        }
    }
    0
}

static VTABLE: Vtable = Vtable {
    query,
    add_ref,
    release,
    create,
    lock,
};

pub struct LocalServer {
    pub class: String,
    cookie: Option<u32>,
    factory: *mut c_void,
    creations: Arc<AtomicU32>,
    // COM initialization and teardown must stay on the registering thread.
    _thread: PhantomData<Rc<()>>,
}

impl LocalServer {
    pub fn new() -> Self {
        Self::with_class(&unique_class())
    }

    pub fn with_class(class: &str) -> Self {
        let class_guid = GUID::from_u128(u128::from_str_radix(class, 16).unwrap());
        // SAFETY: the test thread has no previous COM initialization.
        assert!(unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) } >= 0);
        let creations = Arc::new(AtomicU32::new(0));
        let factory = Box::into_raw(Box::new(Factory {
            vtable: &VTABLE,
            references: AtomicU32::new(1),
            creations: creations.clone(),
        }))
        .cast();
        let mut server = Self {
            class: class.to_owned(),
            cookie: None,
            factory,
            creations,
            _thread: PhantomData,
        };
        let mut cookie = 0;
        // Register only a running local server, not an in-process class or an
        // executable to be launched by SCM. The unique cookie owns revocation.
        assert_eq!(
            unsafe {
                CoRegisterClassObject(
                    &class_guid,
                    factory,
                    CLSCTX_LOCAL_SERVER,
                    REGCLS_MULTI_SEPARATE as u32,
                    &mut cookie,
                )
            },
            0
        );
        server.cookie = Some(cookie);
        server
    }

    pub fn creations(&self) -> u32 {
        self.creations.load(Ordering::SeqCst)
    }

    pub fn cleanup(&mut self) {
        if let Some(cookie) = self.cookie {
            assert_eq!(unsafe { CoRevokeClassObject(cookie) }, 0);
            self.cookie = None;
        }
    }
}

pub fn unique_class() -> String {
    let mut class = GUID::from_u128(0);
    assert_eq!(unsafe { CoCreateGuid(&mut class) }, 0);
    format!(
        "{:08x}{:04x}{:04x}{:016x}",
        class.data1,
        class.data2,
        class.data3,
        u64::from_be_bytes(class.data4)
    )
}

// This callback belongs only to the Rust library test executable. COM appends
// -Embedding to LocalServer32 commands; dispatch before libtest parses it.
// No production binary contains this entry point or accepts these arguments.
#[used]
#[unsafe(link_section = ".CRT$XCU")]
static STANDALONE_SERVER_INIT: extern "C" fn() = {
    extern "C" fn init() {
        let arguments: Vec<_> = std::env::args_os().collect();
        if arguments
            .get(1)
            .is_none_or(|arg| arg != "--tapid-test-com-server")
        {
            return;
        }
        let result = std::panic::catch_unwind(|| {
            assert_eq!(arguments.len(), 5);
            assert_eq!(arguments[4], "-Embedding");
            let mut server = LocalServer::with_class(arguments[2].to_str().unwrap());
            let root = std::path::Path::new(&arguments[3]);
            // Parent owns this disposable directory. Create-new rejects stale
            // readiness evidence instead of silently overwriting it.
            use std::io::Write;
            let mut ready = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join("ready"))
                .unwrap();
            writeln!(ready, "{}", std::process::id()).unwrap();
            drop(ready);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
            while !root.join("stop").exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "standalone COM server timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            server.cleanup();
            std::fs::write(root.join("completed"), server.creations().to_string()).unwrap();
            println!("com:standalone:creations={}", server.creations());
        });
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    init
};

impl Drop for LocalServer {
    fn drop(&mut self) {
        if let Some(cookie) = self.cookie.take() {
            // Best effort during assertion unwinding; normal cleanup checks HRESULT.
            unsafe { CoRevokeClassObject(cookie) };
        }
        // Release our reference after revocation, then balance this thread's COM.
        unsafe {
            release(self.factory);
            CoUninitialize();
        }
    }
}

pub fn probe(marker: &str) {
    let Ok(class) = std::env::var("TAPID_TEST_COM_CLASS") else {
        return;
    };
    let class = GUID::from_u128(u128::from_str_radix(&class, 16).unwrap());
    assert!(unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) } >= 0);
    let mut object = std::ptr::null_mut();
    // Standard IUnknown marshaling only, from a distinct helper process.
    let result = unsafe {
        CoCreateInstance(
            &class,
            std::ptr::null_mut(),
            CLSCTX_LOCAL_SERVER,
            &UNKNOWN,
            &mut object,
        )
    };
    if result >= 0 {
        assert!(!object.is_null());
        // Returned pointer is a COM proxy, never the server's Rust Factory.
        unsafe {
            let vtable = *object.cast::<*const UnknownVtable>();
            ((*vtable).release)(object);
        }
    }
    unsafe { CoUninitialize() };
    if matches!(marker, "provider-host" | "com-descendant-host") {
        assert_eq!(result, 0, "host local-server activation: {result:#010x}");
    }
    if marker == "com-revoked" {
        assert_eq!(
            result as u32, 0x80040154,
            "revoked class must be unavailable"
        );
    }
    let scope = if matches!(marker, "com-descendant" | "com-descendant-host") {
        "descendant"
    } else {
        "local-server"
    };
    match result as u32 {
        0 => println!("com:{scope}:activated"),
        0x80070005 => println!("com:{scope}:access-denied"),
        0x80040154 => println!("com:{scope}:class-unavailable"),
        _ => panic!("unexpected local-server activation result: {result:#010x}"),
    }
}
