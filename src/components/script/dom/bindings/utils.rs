/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use dom::bindings::codegen::PrototypeList;
use script_task::task_from_context;

use std::cast;
use std::hashmap::HashMap;
use std::libc;
use std::ptr;
use std::ptr::{null, to_unsafe_ptr};
use std::result;
use std::str;
use std::sys;
use std::uint;
use js::glue::*;
use js::glue::{DefineFunctionWithReserved, GetObjectJSClass, RUST_OBJECT_TO_JSVAL};
use js::glue::{js_IsObjectProxyClass, js_IsFunctionProxyClass, IsProxyHandlerFamily};
use js::glue::{PROPERTY_STUB, STRICT_PROPERTY_STUB, ENUMERATE_STUB, CONVERT_STUB, RESOLVE_STUB};
use js::jsapi::{JS_AlreadyHasOwnProperty, JS_NewObject, JS_NewFunction};
use js::jsapi::{JS_DefineProperties, JS_WrapValue, JS_ForwardGetPropertyTo};
use js::jsapi::{JS_EncodeString, JS_free, JS_GetStringCharsAndLength};
use js::jsapi::{JS_GetClass, JS_LinkConstructorAndPrototype};
use js::jsapi::{JS_GetFunctionPrototype, JS_InternString, JS_GetFunctionObject};
use js::jsapi::{JS_HasPropertyById, JS_GetPrototype, JS_GetGlobalForObject};
use js::jsapi::{JS_NewStringCopyN, JS_DefineFunctions, JS_DefineProperty};
use js::jsapi::{JS_ValueToString, JS_GetReservedSlot, JS_SetReservedSlot};
use js::jsapi::{JSContext, JSObject, JSBool, jsid, JSClass, JSNative};
use js::jsapi::{JSFunctionSpec, JSPropertySpec, JSVal, JSPropertyDescriptor};
use js::jsfriendapi::bindgen::JS_NewObjectWithUniqueType;
use js::rust::Compartment;
use js::{JSCLASS_HAS_RESERVED_SLOTS, JSPROP_ENUMERATE, JSVAL_NULL};
use js::{JSPROP_PERMANENT, JSID_VOID, JSPROP_NATIVE_ACCESSORS, JSPROP_GETTER};
use js::{JSPROP_SETTER, JSVAL_VOID, JSVAL_TRUE, JSVAL_FALSE};
use js::{JS_THIS_OBJECT, JSFUN_CONSTRUCTOR, JS_CALLEE, JSPROP_READONLY};
use js;

static TOSTRING_CLASS_RESERVED_SLOT: u64 = 0;
static TOSTRING_NAME_RESERVED_SLOT: u64 = 1;

struct GlobalStaticData {
    proxy_handlers: HashMap<uint, *libc::c_void>,
    attribute_ids: HashMap<uint, ~[jsid]>,
    method_ids: HashMap<uint, ~[jsid]>,
    constant_ids: HashMap<uint, ~[jsid]>
}

pub fn GlobalStaticData() -> GlobalStaticData {
    GlobalStaticData {
        proxy_handlers: HashMap::new(),
        attribute_ids: HashMap::new(),
        method_ids: HashMap::new(),
        constant_ids: HashMap::new()
    }
}

extern fn InterfaceObjectToString(cx: *JSContext, _argc: uint, vp: *mut JSVal) -> JSBool {
  unsafe {
    let callee = RUST_JSVAL_TO_OBJECT(*JS_CALLEE(cx, cast::transmute(&vp)));
    let obj = JS_THIS_OBJECT(cx, cast::transmute(&vp));
    if obj.is_null() {
        //XXXjdm figure out JSMSG madness
        /*JS_ReportErrorNumber(cx, js_GetErrorMessage, NULL, JSMSG_CANT_CONVERT_TO,
                             "null", "object");*/
        return 0;
    }

    let v = GetFunctionNativeReserved(callee, TOSTRING_CLASS_RESERVED_SLOT);
    let clasp: *JSClass = cast::transmute(RUST_JSVAL_TO_PRIVATE(*v));

    let v = GetFunctionNativeReserved(callee, TOSTRING_NAME_RESERVED_SLOT);

    if GetObjectJSClass(obj) != clasp {
      /*let jsname: *JSString = RUST_JSVAL_TO_STRING(*v);
      let length = 0;
      let name = JS_GetInternedStringCharsAndLength(jsname, &length);*/
        //XXXjdm figure out JSMSG madness
        /*JS_ReportErrorNumber(cx, js_GetErrorMessage, NULL, JSMSG_INCOMPATIBLE_PROTO,
                             NS_ConvertUTF16toUTF8(name).get(), "toString",
                             "object");*/
        return 0;
    }

    let name = jsval_to_str(cx, *v).get();
    let retval = str(~"function " + name + "() {\n    [native code]\n}");
    *vp = domstring_to_jsval(cx, &retval);
    return 1;
  }
}

#[deriving(Clone)]
pub enum DOMString {
    str(~str),
    null_string
}

impl DOMString {
    pub fn to_str(&self) -> ~str {
        match *self {
          str(ref s) => s.clone(),
          null_string => ~""
        }
    }
}

pub struct JSManaged<T> {
    //XXXjdm Needs rooting
    wrapper: *JSObject
}

impl<T> JSManaged<T> {
    fn sanity_check_impl(available: u32) {
        debug!("%u vs %u", sys::size_of::<T>(), available as uint * sys::size_of::<JSVal>());
        assert!(sys::size_of::<T>() <= available as uint * sys::size_of::<JSVal>());
    }

    pub fn sanity_check() {
        JSManaged::sanity_check_impl::<T>(MAX_FIXED_SLOTS - DOM_OBJECT_SLOT);
    }

    pub fn sanity_check_proxy() {
        JSManaged::sanity_check_impl::<T>(MAX_FIXED_SLOTS - DOM_PROXY_OBJECT_SLOT);
    }
}

impl<T> Clone for JSManaged<T> {
    fn clone(&self) -> JSManaged<T> {
        JSManaged {
            wrapper: self.wrapper
        }
    }
}

impl<T: CacheableWrapper> JSManaged<T> {
    pub fn new(mut wrapped: T) -> JSManaged<T> {
        let cache: *mut WrapperCache = wrapped.get_wrappercache();
        let mut wrapper = unsafe { (*cache).get_wrapper() };
        if wrapper.is_null() {
            wrapper = wrapped.init_wrapper();
        }
        assert!(wrapper != ptr::null());
        JSManaged {
            wrapper: wrapper
        }
    }

    pub fn from_raw(wrapper: *JSObject) -> JSManaged<T> {
        assert!(wrapper != ptr::null());
        JSManaged {
            wrapper: wrapper
        }
    }

    pub fn with_imm<R>(&self, cb: &fn(obj: &T) -> R) -> R {
        unsafe {
            assert!(self.wrapper != ptr::null());
            let inner = unwrap::<&T>(self.wrapper);
            cb(inner)
        }
    }

    pub fn with_mut<R>(&self, cb: &fn(obj: &mut T) -> R) -> R {
        unsafe {
            assert!(self.wrapper != ptr::null());
            let inner = unwrap::<&mut T>(self.wrapper);
            cb(inner)
        }
    }
}

fn is_dom_class(clasp: *JSClass) -> bool {
    unsafe {
        ((*clasp).flags & js::JSCLASS_IS_INLINE_DOMJSCLASS) != 0
    }
}

fn is_dom_proxy(obj: *JSObject) -> bool {
    unsafe {
        (js_IsObjectProxyClass(obj) || js_IsFunctionProxyClass(obj)) &&
            IsProxyHandlerFamily(obj)
    }
}

pub unsafe fn unwrap<T>(obj: *JSObject) -> T {
    debug!("unwrapping 0x%x", obj as uint);
    let clasp = JS_GetClass(obj);
    let slot = if is_dom_class(clasp) {
        DOM_OBJECT_SLOT
    } else {
        assert!(is_dom_proxy(obj));
        DOM_PROXY_OBJECT_SLOT
    } as u32;
    let val: *T = GetInlineStorage(obj, slot) as *T;
    debug!("storage at 0x%x", ptr::to_unsafe_ptr(&*val) as uint);
    cast::transmute(val)
}

pub unsafe fn get_dom_class(obj: *JSObject) -> Result<DOMClass, ()> {
    let clasp = JS_GetClass(obj);
    if is_dom_class(clasp) {
        debug!("plain old dom object");
        let domjsclass: *DOMJSClass = cast::transmute(clasp);
        return Ok((*domjsclass).dom_class);
    }
    if is_dom_proxy(obj) {
        debug!("proxy dom object");
        let dom_class: *DOMClass = cast::transmute(GetProxyHandlerExtra(obj));
        return Ok(*dom_class);
    }
    debug!("not a dom object");
    return Err(());
}

pub fn can_unwrap_object(obj: *JSObject, proto_id: PrototypeList::id::ID, proto_depth: uint) -> Result<(), ()> {
   unsafe {
        do get_dom_class(obj).chain |dom_class| {
            if dom_class.interface_chain[proto_depth] == proto_id {
                debug!("good prototype");
                Ok(())
            } else {
                debug!("bad prototype");
                Err(())
            }
        }
    }
}

//XXX very incomplete
pub fn jsval_to_str(cx: *JSContext, v: JSVal) -> Result<~str, ()> {
    unsafe {
        let jsstr;
        if RUST_JSVAL_IS_STRING(v) == 1 {
            jsstr = RUST_JSVAL_TO_STRING(v)
        } else {
            jsstr = JS_ValueToString(cx, v);
            if jsstr.is_null() {
                return Err(());
            }
        }

        let strbuf = JS_EncodeString(cx, jsstr);
        let buf = str::raw::from_buf(strbuf as *u8);
        JS_free(cx, strbuf as *libc::c_void);
        Ok(buf)
    }
}

pub unsafe fn domstring_to_jsval(cx: *JSContext, string: &DOMString) -> JSVal {
    match string {
      &null_string => {
        JSVAL_NULL
      }
      &str(ref s) => {
        str::as_buf(*s, |buf, len| {
            let cbuf = cast::transmute(buf);
            RUST_STRING_TO_JSVAL(JS_NewStringCopyN(cx, cbuf, len as libc::size_t))
        })
      }
    }
}

pub fn get_compartment(cx: *JSContext) -> @mut Compartment {
    unsafe {
        let script_context = task_from_context(cx);
        let compartment = (*script_context).js_compartment;
        assert!(cx == compartment.cx.ptr);
        compartment
    }
}

extern fn has_instance(_cx: *JSContext, obj: **JSObject, v: *JSVal, bp: *mut JSBool) -> JSBool {
    //XXXjdm this is totally broken for non-object values
    unsafe {
        let mut o = RUST_JSVAL_TO_OBJECT(*v);
        let obj = *obj;
        *bp = 0;
        while o.is_not_null() {
            if o == obj {
                *bp = 1;
                break;
            }
            o = JS_GetPrototype(o);
        }
        return 1;
    }
}

pub fn prototype_jsclass(name: ~str) -> @fn(compartment: @mut Compartment) -> JSClass {
    let f: @fn(@mut Compartment) -> JSClass = |compartment: @mut Compartment| {
        unsafe {
            JSClass {
                name: compartment.add_name(copy name),
                flags: 0,
                addProperty: GetJSClassHookStubPointer(PROPERTY_STUB) as *u8,
                delProperty: GetJSClassHookStubPointer(PROPERTY_STUB) as *u8,
                getProperty: GetJSClassHookStubPointer(PROPERTY_STUB) as *u8,
                setProperty: GetJSClassHookStubPointer(STRICT_PROPERTY_STUB) as *u8,
                enumerate: GetJSClassHookStubPointer(ENUMERATE_STUB) as *u8,
                resolve: GetJSClassHookStubPointer(RESOLVE_STUB) as *u8,
                convert: GetJSClassHookStubPointer(CONVERT_STUB) as *u8,
                finalize: null(),
                checkAccess: null(),
                call: null(),
                hasInstance: has_instance,
                construct: null(),
                trace: null(),
                reserved: (null(), null(), null(), null(), null(),  // 05
                           null(), null(), null(), null(), null(),  // 10
                           null(), null(), null(), null(), null(),  // 15
                           null(), null(), null(), null(), null(),  // 20
                           null(), null(), null(), null(), null(),  // 25
                           null(), null(), null(), null(), null(),  // 30
                           null(), null(), null(), null(), null(),  // 35
                           null(), null(), null(), null(), null())  // 40
            }
        }
    };
    return f;
}

pub fn instance_jsclass(name: ~str, finalize: *u8, trace: *u8)
                     -> @fn(compartment: @mut Compartment) -> JSClass {
    let f: @fn(@mut Compartment) -> JSClass = |compartment: @mut Compartment| {
        unsafe {
            JSClass {
                name: compartment.add_name(copy name),
                flags: JSCLASS_HAS_RESERVED_SLOTS(16) | js::JSCLASS_IS_INLINE_DOMJSCLASS,
                addProperty: GetJSClassHookStubPointer(PROPERTY_STUB) as *u8,
                delProperty: GetJSClassHookStubPointer(PROPERTY_STUB) as *u8,
                getProperty: GetJSClassHookStubPointer(PROPERTY_STUB) as *u8,
                setProperty: GetJSClassHookStubPointer(STRICT_PROPERTY_STUB) as *u8,
                enumerate: GetJSClassHookStubPointer(ENUMERATE_STUB) as *u8,
                resolve: GetJSClassHookStubPointer(RESOLVE_STUB) as *u8,
                convert: GetJSClassHookStubPointer(CONVERT_STUB) as *u8,
                finalize: finalize,
                checkAccess: null(),
                call: null(),
                hasInstance: has_instance,
                construct: null(),
                trace: trace,
                reserved: (null(), null(), null(), null(), null(),  // 05
                           null(), null(), null(), null(), null(),  // 10
                           null(), null(), null(), null(), null(),  // 15
                           null(), null(), null(), null(), null(),  // 20
                           null(), null(), null(), null(), null(),  // 25
                           null(), null(), null(), null(), null(),  // 30
                           null(), null(), null(), null(), null(),  // 35
                           null(), null(), null(), null(), null())  // 40
            }
        }
    };
    return f;
}

// FIXME: A lot of string copies here
pub fn define_empty_prototype(name: ~str, proto: Option<~str>, compartment: @mut Compartment)
    -> js::rust::jsobj {
    compartment.register_class(prototype_jsclass(copy name));

    //TODO error checking
    let obj = result::unwrap(
        match proto {
            Some(s) => compartment.new_object_with_proto(copy name,
                                                         s, 
                                                         compartment.global_obj.ptr),
            None => compartment.new_object(copy name, null(), compartment.global_obj.ptr)
        });

    unsafe {
        compartment.define_property(copy name, RUST_OBJECT_TO_JSVAL(obj.ptr),
                                    GetJSClassHookStubPointer(PROPERTY_STUB) as *u8,
                                    GetJSClassHookStubPointer(STRICT_PROPERTY_STUB) as *u8,
                                    JSPROP_ENUMERATE);
        compartment.stash_global_proto(name, obj);
        return obj;
    }
}

// We use slot 0 for holding the raw object.  This is safe for both
// globals and non-globals.
pub static DOM_OBJECT_SLOT: u32 = 0;
pub static DOM_PROXY_OBJECT_SLOT: u32 = 3;
pub static MAX_FIXED_SLOTS: u32 = 16;

// NOTE: This is baked into the Ion JIT as 0 in codegen for LGetDOMProperty and
// LSetDOMProperty. Those constants need to be changed accordingly if this value
// changes.
static DOM_PROTO_INSTANCE_CLASS_SLOT: u32 = 0;

// All DOM globals must have a slot at DOM_PROTOTYPE_SLOT.
pub static DOM_PROTOTYPE_SLOT: u32 = js::JSCLASS_GLOBAL_SLOT_COUNT;

// NOTE: This is baked into the Ion JIT as 0 in codegen for LGetDOMProperty and
// LSetDOMProperty. Those constants need to be changed accordingly if this value
// changes.
static JSCLASS_DOM_GLOBAL: u32 = js::JSCLASS_USERBIT1;

pub struct NativeProperties {
    staticMethods: *JSFunctionSpec,
    staticMethodIds: *jsid,
    staticMethodsSpecs: *JSFunctionSpec,
    staticAttributes: *JSPropertySpec,
    staticAttributeIds: *jsid,
    staticAttributeSpecs: *JSPropertySpec,
    methods: *JSFunctionSpec,
    methodIds: *jsid,
    methodsSpecs: *JSFunctionSpec,
    attributes: *JSPropertySpec,
    attributeIds: *jsid,
    attributeSpecs: *JSPropertySpec,
    unforgeableAttributes: *JSPropertySpec,
    unforgeableAttributeIds: *jsid,
    unforgeableAttributeSpecs: *JSPropertySpec,
    constants: *ConstantSpec,
    constantIds: *jsid,
    constantSpecs: *ConstantSpec
}

pub struct NativePropertyHooks {
    resolve_own_property: *u8,
    resolve_property: *u8,
    enumerate_own_properties: *u8,
    enumerate_properties: *u8,
    proto_hooks: *NativePropertyHooks
}

pub struct JSNativeHolder {
    native: js::jsapi::JSNative,
    propertyHooks: *NativePropertyHooks
}

pub enum ConstantVal {
    IntVal(i32),
    UintVal(u32),
    DoubleVal(f64),
    BoolVal(bool),
    NullVal,
    VoidVal
}

pub struct ConstantSpec {
    name: *libc::c_char,
    value: ConstantVal
}

pub struct DOMClass {
    // A list of interfaces that this object implements, in order of decreasing
    // derivedness.
    interface_chain: [PrototypeList::id::ID, ..3 /*max prototype chain length*/],

    unused: bool, // DOMObjectIsISupports (always false)
    native_hooks: *NativePropertyHooks
}

pub struct DOMJSClass {
    base: JSClass,
    dom_class: DOMClass
}

pub fn GetProtoOrIfaceArray(global: *JSObject) -> **JSObject {
    unsafe {
        /*assert ((*JS_GetClass(global)).flags & JSCLASS_DOM_GLOBAL) != 0;*/
        let raw = RUST_JSVAL_TO_PRIVATE(JS_GetReservedSlot(global, DOM_PROTOTYPE_SLOT));
        let boxed: @mut [*JSObject, ..1] = cast::transmute(raw);
        let retVal = ptr::to_unsafe_ptr(&boxed[0]);
        cast::forget(boxed);
        retVal
    }
}

pub fn CreateInterfaceObjects2(cx: *JSContext, global: *JSObject, receiver: *JSObject,
                               protoProto: *JSObject, protoClass: *JSClass,
                               constructorClass: *JSClass, constructor: JSNative,
                               ctorNargs: u32,
                               domClass: *DOMClass,
                               methods: *JSFunctionSpec,
                               properties: *JSPropertySpec,
                               constants: *ConstantSpec,
                               staticMethods: *JSFunctionSpec,
                               name: &str) -> *JSObject {
    let mut proto = ptr::null();
    if protoClass.is_not_null() {
        proto = CreateInterfacePrototypeObject(cx, global, protoProto,
                                               protoClass, methods,
                                               properties, constants);
        if proto.is_null() {
            return ptr::null();
        }

        unsafe {
            JS_SetReservedSlot(proto, DOM_PROTO_INSTANCE_CLASS_SLOT,
                               RUST_PRIVATE_TO_JSVAL(domClass as *libc::c_void));
        }
    }

    let mut interface = ptr::null();
    if constructorClass.is_not_null() || constructor.is_not_null() {
        interface = do str::as_c_str(name) |s| {
            CreateInterfaceObject(cx, global, receiver, constructorClass,
                                  constructor, ctorNargs, proto,
                                  staticMethods, constants, s)
        };
        if interface.is_null() {
            return ptr::null();
        }
    }

    if protoClass.is_not_null() {
        proto
    } else {
        interface
    }
}

fn CreateInterfaceObject(cx: *JSContext, global: *JSObject, receiver: *JSObject,
                         constructorClass: *JSClass, constructorNative: JSNative,
                         ctorNargs: u32, proto: *JSObject,
                         staticMethods: *JSFunctionSpec,
                         constants: *ConstantSpec,
                         name: *libc::c_char) -> *JSObject {
    unsafe {
        let constructor = if constructorClass.is_not_null() {
            let functionProto = JS_GetFunctionPrototype(cx, global);
            if functionProto.is_null() {
                ptr::null()
            } else {
                JS_NewObject(cx, constructorClass, functionProto, global)
            }
        } else {
            assert!(constructorNative.is_not_null());
            let fun = JS_NewFunction(cx, constructorNative, ctorNargs,
                                     JSFUN_CONSTRUCTOR, global, name);
            if fun.is_null() {
                ptr::null()
            } else {
                JS_GetFunctionObject(fun)
            }
        };

        if constructor.is_null() {
            return ptr::null();
        }

        if staticMethods.is_not_null() &&
            !DefineMethods(cx, constructor, staticMethods) {
            return ptr::null();
        }

        if constructorClass.is_not_null() {
            let toString = do str::as_c_str("toString") |s| {
                DefineFunctionWithReserved(cx, constructor, s,
                                           InterfaceObjectToString,
                                           0, 0)
            };
            if toString.is_null() {
                return ptr::null();
            }

            let toStringObj = JS_GetFunctionObject(toString);
            SetFunctionNativeReserved(toStringObj, TOSTRING_CLASS_RESERVED_SLOT,
                                      &RUST_PRIVATE_TO_JSVAL(constructorClass as *libc::c_void));
            let s = JS_InternString(cx, name);
            if s.is_null() {
                return ptr::null();
            }
            SetFunctionNativeReserved(toStringObj, TOSTRING_NAME_RESERVED_SLOT,
                                      &RUST_STRING_TO_JSVAL(s));
        }

        if constants.is_not_null() &&
            !DefineConstants(cx, constructor, constants) {
            return ptr::null();
        }

        if proto.is_not_null() && JS_LinkConstructorAndPrototype(cx, constructor, proto) == 0 {
            return ptr::null();
        }

        let alreadyDefined = 0;
        if JS_AlreadyHasOwnProperty(cx, receiver, name, &alreadyDefined) == 0 {
            return ptr::null();
        }

        if alreadyDefined == 0 &&
            JS_DefineProperty(cx, receiver, name, RUST_OBJECT_TO_JSVAL(constructor),
                              ptr::null(), ptr::null(), 0) == 0 {
            return ptr::null();
        }

        return constructor;
    }
}

fn DefineConstants(cx: *JSContext, obj: *JSObject, constants: *ConstantSpec) -> bool {
    let mut i = 0;
    loop {
        unsafe {
            let spec = *constants.offset(i);
            if spec.name.is_null() {
                return true;
            }
            let jsval = match spec.value {
                NullVal => JSVAL_NULL,
                IntVal(i) => RUST_INT_TO_JSVAL(i),
                UintVal(u) => RUST_UINT_TO_JSVAL(u),
                DoubleVal(d) => RUST_DOUBLE_TO_JSVAL(d),
                BoolVal(b) if b => JSVAL_TRUE,
                BoolVal(_) => JSVAL_FALSE,
                VoidVal => JSVAL_VOID
            };
            if JS_DefineProperty(cx, obj, spec.name,
                                 jsval, ptr::null(),
                                 ptr::null(),
                                 JSPROP_ENUMERATE | JSPROP_READONLY |
                                 JSPROP_PERMANENT) == 0 {
                return false;
            }
        }
        i += 1;
    }
}

fn DefineMethods(cx: *JSContext, obj: *JSObject, methods: *JSFunctionSpec) -> bool {
    unsafe {
        JS_DefineFunctions(cx, obj, methods) != 0
    }
}

fn DefineProperties(cx: *JSContext, obj: *JSObject, properties: *JSPropertySpec) -> bool {
    unsafe {
        JS_DefineProperties(cx, obj, properties) != 0
    }
}

fn CreateInterfacePrototypeObject(cx: *JSContext, global: *JSObject,
                                  parentProto: *JSObject, protoClass: *JSClass,
                                  methods: *JSFunctionSpec,
                                  properties: *JSPropertySpec,
                                  constants: *ConstantSpec) -> *JSObject {
    unsafe {
        let ourProto = JS_NewObjectWithUniqueType(cx, protoClass, parentProto, global);
        if ourProto.is_null() {
            return ptr::null();
        }

        if methods.is_not_null() && !DefineMethods(cx, ourProto, methods) {
            return ptr::null();
        }

        if properties.is_not_null() && !DefineProperties(cx, ourProto, properties) {
            return ptr::null();
        }

        if constants.is_not_null() && !DefineConstants(cx, ourProto, constants) {
            return ptr::null();
        }

        return ourProto;
    }
}

pub extern fn ThrowingConstructor(_cx: *JSContext, _argc: uint, _vp: *JSVal) -> JSBool {
    //XXX should trigger exception here
    return 0;
}

pub fn initialize_global(global: *JSObject) {
    let protoArray = @mut ([0 as *JSObject, ..22]); //XXXjdm PrototyepList::id::_ID_Count
    unsafe {
        JS_SetReservedSlot(global,
                           DOM_PROTOTYPE_SLOT,
                           RUST_PRIVATE_TO_JSVAL(cast::transmute(protoArray)));
    }
}

pub trait CacheableWrapper {
    fn get_wrappercache(&mut self) -> &mut WrapperCache;
    fn wrap_object_shared(self, cx: *JSContext, scope: *JSObject) -> *JSObject;
    fn init_wrapper(self) -> *JSObject;
}

pub struct WrapperCache {
    wrapper: *JSObject
}

impl WrapperCache {
    pub fn get_wrapper(&self) -> *JSObject {
        unsafe { cast::transmute(self.wrapper) }
    }

    pub fn set_wrapper(&mut self, wrapper: *JSObject) {
        self.wrapper = wrapper;
    }

    pub fn get_rootable(&self) -> **JSObject {
        return to_unsafe_ptr(&self.wrapper);
    }

    pub fn new() -> WrapperCache {
        WrapperCache {
            wrapper: ptr::null()
        }
    }
}

pub fn WrapNewBindingObject(cx: *JSContext, _scope: *JSObject,
                            obj: *JSObject,
                            vp: *mut JSVal) -> bool {
  unsafe {
    assert!(!obj.is_null());
    *vp = RUST_OBJECT_TO_JSVAL(obj);
    return JS_WrapValue(cx, cast::transmute(vp)) != 0;
  }
}

pub trait BindingObject {
    fn GetParentObject(&self, cx: *JSContext) -> *JSObject;
}

pub fn GetPropertyOnPrototype(cx: *JSContext, proxy: *JSObject, id: jsid, found: *mut bool,
                              vp: *JSVal) -> bool {
    unsafe {
      //let proto = GetObjectProto(proxy);
      let proto = JS_GetPrototype(proxy);
      if proto.is_null() {
          *found = false;
          return true;
      }
      let hasProp = 0;
      if JS_HasPropertyById(cx, proto, id, ptr::to_unsafe_ptr(&hasProp)) == 0 {
          return false;
      }
      *found = hasProp != 0;
      let no_output = vp.is_null();
      if hasProp == 0 || no_output {
          return true;
      }

      JS_ForwardGetPropertyTo(cx, proto, id, proxy, vp) != 0
  }
}

pub fn GetArrayIndexFromId(_cx: *JSContext, id: jsid) -> Option<u32> {
    unsafe {
        if RUST_JSID_IS_INT(id) != 0 {
            return Some(RUST_JSID_TO_INT(id) as u32);
        }
        return None;
    }
    // if id is length atom, -1, otherwise
    /*return if JSID_IS_ATOM(id) {
        let atom = JSID_TO_ATOM(id);
        //let s = *GetAtomChars(id);
        if s > 'a' && s < 'z' {
            return -1;
        }

        let i = 0;
        let str = AtomToLinearString(JSID_TO_ATOM(id));
        return if StringIsArray(str, &mut i) != 0 { i } else { -1 }
    } else {
        IdToInt32(cx, id);
    }*/
}

pub fn XrayResolveProperty(cx: *JSContext,
                           wrapper: *JSObject,
                           id: jsid,
                           desc: *mut JSPropertyDescriptor,
                           _methods: Option<~[(JSFunctionSpec, jsid)]>,
                           attributes: Option<~[(JSPropertySpec, jsid)]>,
                           _constants: Option<~[(ConstantSpec, jsid)]>) -> bool
{
  unsafe {
    match attributes {
        Some(attrs) => {
            for attrs.iter().advance |&elem| {
                let (attr, attr_id) = elem;
                if attr_id == JSID_VOID || attr_id != id {
                    loop;
                }

                (*desc).attrs = (attr.flags & !(JSPROP_NATIVE_ACCESSORS as u8)) as u32;
                let global = JS_GetGlobalForObject(cx, wrapper);
                let fun = JS_NewFunction(cx, attr.getter.op, 0, 0, global, ptr::null());
                if fun.is_null() {
                    return false;
                }

                RUST_SET_JITINFO(fun, attr.getter.info);
                let funobj = JS_GetFunctionObject(fun);
                (*desc).getter = funobj as *u8;
                (*desc).attrs |= JSPROP_GETTER;
                if attr.setter.op.is_not_null() {
                    let fun = JS_NewFunction(cx, attr.setter.op, 1, 0, global, ptr::null());
                    if fun.is_null() {
                        return false
                    }

                    RUST_SET_JITINFO(fun, attr.setter.info);
                    let funobj = JS_GetFunctionObject(fun);
                    (*desc).setter = funobj as *u8;
                    (*desc).attrs |= JSPROP_SETTER;
                } else {
                    (*desc).setter = ptr::null();
                }
            }
        }
        None => ()
    }
    return true;
  }
}

fn InternJSString(cx: *JSContext, chars: *libc::c_char) -> Option<jsid> {
    unsafe {
        let s = JS_InternString(cx, chars);
        if s.is_not_null() {
            Some(RUST_INTERNED_STRING_TO_JSID(cx, s))
        } else {
            None
        }
    }
}

pub fn InitIds(cx: *JSContext, specs: &[JSPropertySpec], ids: &mut [jsid]) -> bool {
    let mut rval = true;
    for specs.iter().enumerate().advance |(i, spec)| {
        if spec.name.is_null() == true {
            break;
        }
        match InternJSString(cx, spec.name) {
            Some(id) => ids[i] = id,
            None => {
                rval = false;
                return false;
            }
        }
    }
    rval
}

pub enum Error {
    FailureUnknown
}

pub type ErrorResult = Result<(), Error>;

pub struct EnumEntry {
    value: &'static str,
    length: uint
}

pub fn FindEnumStringIndex(cx: *JSContext,
                           v: JSVal,
                           values: &[EnumEntry]) -> Result<uint, ()> {
    unsafe {
        let jsstr = JS_ValueToString(cx, v);
        if jsstr.is_null() {
            return Err(());
        }
        let length = 0;
        let chars = JS_GetStringCharsAndLength(cx, jsstr, ptr::to_unsafe_ptr(&length));
        if chars.is_null() {
            return Err(());
        }
        for values.iter().enumerate().advance |(i, value)| {
            if value.length != length as uint {
                loop;
            }
            let mut equal = true;
            for uint::iterate(0, length as uint) |j| {
                if value.value[j] as u16 != *chars.offset(j) {
                    equal = false;
                    break;
                }
            };

            if equal {
                return Ok(i);
            }
        }

        return Err(()); //XXX pass in behaviour for value not found
    }
}
