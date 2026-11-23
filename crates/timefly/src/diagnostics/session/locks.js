// Register under an origin-wide lock so simultaneous launches cannot lose each other's IDs.
// The session lock stays held even in a suspended tab. Destroying its document releases it.
export function withSessionLocks(id, register) {
    const locks = navigator.locks;
    if (!locks) {
        register(null);
        return;
    }

    const prefix = "timefly.live.";
    let registered = false;
    locks.request(prefix + id, async () => {
        await locks.request("timefly.registry", async () => {
            const { held } = await locks.query();
            const live = held.filter(lock => lock.name.startsWith(prefix))
                .map(lock => lock.name.slice(prefix.length));
            registered = true;
            register(live);
        });
        await new Promise(() => {});
    }).catch(error => {
        if (registered) throw error;
        // Storage policy can disable locks even when the API is present.
        register(null);
    });
}
