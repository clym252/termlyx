//! iOS side of background keep-alive (`crate::keepalive`): a
//! `CLLocationManager` at the lowest accuracy whose updates keep iOS from
//! suspending Tether while sessions are open (the `location` background mode
//! and `NSLocationWhenInUseUsageDescription` are in `gen/apple/project.yml`).
//!
//! The manager is created at setup on the main thread and only ever used
//! there: starting, stopping and the permission question are queued onto
//! the main queue (never run inline, so the slot's lock is never taken back
//! from under itself). Its delegate passes permission changes to the slot
//! and drops every location fix without reading it.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_core_location::{
    CLAuthorizationStatus, CLLocation, CLLocationManager, CLLocationManagerDelegate,
    kCLLocationAccuracyThreeKilometers,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};

use crate::keepalive::{KeepAliveSlot, LocationDriver, Permission};

/// Where the delegate reports permission changes (set once, at setup).
static SLOT: OnceLock<Arc<KeepAliveSlot>> = OnceLock::new();

thread_local! {
    /// The manager and its delegate (the manager holds it weakly). Only set
    /// and read on the main thread.
    static MANAGER: RefCell<Option<(Retained<CLLocationManager>, Retained<Delegate>)>> =
        const { RefCell::new(None) };
}

fn permission_of(status: CLAuthorizationStatus) -> Permission {
    if status == CLAuthorizationStatus::AuthorizedWhenInUse
        || status == CLAuthorizationStatus::AuthorizedAlways
    {
        Permission::Granted
    } else if status == CLAuthorizationStatus::NotDetermined {
        Permission::NotDetermined
    } else {
        Permission::Denied
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; `Delegate` has no
    // `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TetherKeepAliveDelegate"]
    struct Delegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the signatures are the protocol's.
    unsafe impl CLLocationManagerDelegate for Delegate {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update_locations(
            &self,
            _manager: &CLLocationManager,
            _locations: &NSArray<CLLocation>,
        ) {
            // Dropped unread: the updates only keep Tether running. Never
            // stored, logged, shown or sent.
        }

        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn did_change_authorization(&self, manager: &CLLocationManager) {
            // SAFETY: a plain property read on the manager that called us.
            let status = unsafe { manager.authorizationStatus() };
            if let Some(slot) = SLOT.get() {
                slot.set_permission(permission_of(status));
            }
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        fn did_fail(&self, _manager: &CLLocationManager, error: &NSError) {
            // The code only (kCLErrorLocationUnknown and the like): no place.
            tracing::debug!(code = error.code(), "location update failed");
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's `init`.
        unsafe { msg_send![super(this), init] }
    }
}

/// Runs `f` with the manager on the main thread, later (queued even when
/// called there). Logs and does nothing if there is no manager.
fn on_main(f: impl FnOnce(&CLLocationManager) + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        let manager = MANAGER.with(|m| m.borrow().as_ref().map(|(manager, _)| manager.clone()));
        match manager {
            Some(manager) => f(&manager),
            None => tracing::warn!("no location manager; keep-alive does nothing"),
        }
    });
}

struct CoreLocation;

impl LocationDriver for CoreLocation {
    fn request_permission(&self) {
        // SAFETY: called on the main thread, where the manager was made.
        on_main(|m| unsafe { m.requestWhenInUseAuthorization() });
    }

    fn set_updating(&self, on: bool, _generation: u64) {
        on_main(move |m| {
            // SAFETY: as above.
            unsafe {
                if on {
                    m.startUpdatingLocation();
                } else {
                    m.stopUpdatingLocation();
                }
            }
        });
    }
}

/// Creates the location manager and installs it as `slot`'s driver. Call
/// once, at app setup (on the main thread). Off the main thread it logs and
/// leaves keep-alive unavailable.
pub fn install(slot: Arc<KeepAliveSlot>) {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("keep-alive set up off the main thread; it stays unavailable");
        return;
    };
    if SLOT.set(slot.clone()).is_err() {
        return;
    }
    // SAFETY: on the main thread. Background updates need the `location`
    // background mode in Info.plist (setting the flag without it raises);
    // `project.yml` has it.
    let (manager, delegate, status) = unsafe {
        let manager = CLLocationManager::new();
        let delegate = Delegate::new(mtm);
        manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        manager.setDesiredAccuracy(kCLLocationAccuracyThreeKilometers);
        manager.setAllowsBackgroundLocationUpdates(true);
        manager.setPausesLocationUpdatesAutomatically(false);
        manager.setShowsBackgroundLocationIndicator(true);
        let status = manager.authorizationStatus();
        (manager, delegate, status)
    };
    MANAGER.with(|m| *m.borrow_mut() = Some((manager, delegate)));
    slot.install(Arc::new(CoreLocation));
    slot.set_permission(permission_of(status));
}
