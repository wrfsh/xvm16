/*
 * VM model
 */

use hypervisor_framework::*;
use std::sync::Arc;
use std::rc::Rc;
use std::mem;
use rlibc::*;

extern "C" {
    fn valloc(size: usize) -> *mut ::std::os::raw::c_void;
}

/*
 * VM allocated memory region
 */
#[derive(Debug)]
pub struct memory_region 
{
    pub data: hv_uvaddr_t,
    pub size: usize,
}

impl memory_region {

    pub fn read_bytes(&self, offset: usize, buf: &mut [u8]) -> usize {
        if offset >= self.size {
            return 0;
        }

        let toread = if offset + buf.len() > self.size {
            self.size - offset
        } else {
            buf.len()
        };

        unsafe {
            memcpy(buf.as_mut_ptr(), (self.data as *const u8).offset(offset as isize), toread);
        }

        toread
    }

    pub fn write_bytes(&self, offset: usize, buf: &[u8]) -> usize {
        if offset >= self.size {
            return 0;
        }

        let towrite = if offset + buf.len() > self.size {
            self.size - offset
        } else {
            buf.len()
        };

        unsafe {
            memcpy((self.data as *mut u8).offset(offset as isize), buf.as_ptr(), towrite);
        }

        towrite
    }
}

#[derive(Debug)]
pub struct memory_mapping
{
    pub region: Arc<memory_region>,
    pub base: hv_gpaddr_t,
    pub flags: hv_memory_flags_t,
}

pub trait io_handler
{
    fn io_read(&self, addr: u16, size: u8) -> IoOperandType;
    fn io_write(&self, addr: u16, data: IoOperandType);
}

pub struct io_region
{
    base: u16,
    size: u8,
    ops: Rc<io_handler>,
}

/*
 * VM state 
 *
 * TODO: drop trait to clean up and call hv_vm_destroy
 * TODO: a better lookup for memory mappings
 */
pub struct vm {
    /* HV vcpu id */
    pub vcpu: hv_vcpuid_t,

    /* Mapped memory regions */
    pub memory: Vec<memory_mapping>,

    /* Registred PIO regions */
    pub io: Vec<io_region>,
}

/*
 * Unsafe heap pointer to global VM state
 *
 * HV framework internally creates a single global VM context for process context which means 
 * that dynamic instanses of this struct don't really make sense.
 *
 * Use get_vm() to safely unwrap this pointer.
 *
 * TODO: We are single threaded now, but remeber to add proper sync in case of multiple threads
 */
static mut VM: Option<*mut vm> = Option::None;

fn get_vm() -> &'static mut vm
{
    unsafe {
        mem::transmute(VM.unwrap())
    }
}

pub fn create()
{
    unsafe {
        let res = hv_vm_create(HV_VM_DEFAULT);
        assert!(res == HV_SUCCESS);

        let vm = vm {
                    vcpu: vcpu_create(),
                    memory: Vec::new(),
                    io: Vec::new()
        };

        VM = Option::Some(mem::transmute(Box::new(vm)));
    }
}

fn alloc_pages(size: usize) -> hv_uvaddr_t 
{
    unsafe {
        let va = valloc(size);
        assert!(!va.is_null());
        return va;
    }
}

pub fn vcpu() -> hv_vcpuid_t
{
    //VM.unwrap().vcpu
    get_vm().vcpu
}

pub fn alloc_memory_region(size: usize) -> Arc<memory_region>
{
    let va = alloc_pages(size);
    Arc::new(memory_region { size: size, data: va })
}

pub fn map_memory_region(base: hv_gpaddr_t, flags: hv_memory_flags_t, region: Arc<memory_region>)
{
    unsafe {
        let res = hv_vm_map(region.data, base, region.size, flags);
        assert!(res == HV_SUCCESS);
    }

    get_vm().memory.push(memory_mapping { region: region, base: base, flags: flags });
}

pub fn find_memory_mapping(addr: hv_gpaddr_t) -> Option<&'static memory_mapping>
{
    for i in &get_vm().memory {
        if addr >= i.base && addr < i.base + i.region.size as u64 {
            return Some(i);
        }
    }

    return None;
}

pub fn read_guest_memory(addr: hv_gpaddr_t, buf: &mut [u8]) -> usize
{
    let mapping = match find_memory_mapping(addr) {
        Some(mapping) => mapping,
        None => return 0,
    };

    assert!(addr >= mapping.base);
    mapping.region.read_bytes((addr - mapping.base) as usize, buf)
}

pub fn vcpu_create() -> hv_vcpuid_t 
{
    unsafe {
        let mut vcpu: hv_vcpuid_t = 0;  
        let res = hv_vcpu_create(&mut vcpu, HV_VCPU_DEFAULT);
        assert!(res == HV_SUCCESS);
        vcpu
    }
}

pub fn run(vcpu: hv_vcpuid_t) -> hv_return_t
{
    unsafe {
        hv_vcpu_run(vcpu)
    }
}

pub fn register_io_region(handler: Rc<io_handler>, base: u16, len: u8)
{
    // TODO: check if range intersects
    get_vm().io.push(io_region {
        ops: handler,
        base: base,
        size: len
    });
}

#[derive(Clone, Copy)]
pub enum IoOperandType {
    byte(u8),
    word(u16),
    dword(u32),
}

#[allow(dead_code)]
impl IoOperandType {
    pub fn unwrap_byte(&self) -> u8 {
        match self {
            &IoOperandType::byte(v) => v,
            _ => panic!(),
        }
    }

    pub fn unwrap_word(&self) -> u16 {
        match self {
            &IoOperandType::word(v) => v,
            _ => panic!(),
        }
    }

    pub fn unwrap_dword(&self) -> u32 {
        match self {
            &IoOperandType::dword(v) => v,
            _ => panic!(),
        }
    }

    pub fn make_unhandled(size: u8) -> IoOperandType {
        match size {
            1 => IoOperandType::byte(0xFF),
            2 => IoOperandType::word(0xFFFF),
            4 => IoOperandType::dword(0xFFFFFFFF),
            _ => panic!(),
        }
    }
}

pub fn handle_io_read(port: u16, size: u8) -> IoOperandType
{
    for i in &mut get_vm().io {
        if port == i.base {
            return i.ops.io_read(port, size);
        }
    }

    panic!("Unhandled IO read from port {:x}", port);
}

pub fn handle_io_write(port: u16, data: IoOperandType)
{
    for i in &mut get_vm().io {
        if port == i.base {
            i.ops.io_write(port, data);
            return;

        }
    }

    panic!("Unhandled IO write to port {:x}", port);
}
