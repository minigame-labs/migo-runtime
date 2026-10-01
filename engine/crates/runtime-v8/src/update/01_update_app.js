import { failMessage } from "ext:host_v8_base/02_async.js";
function updateApp(options = {}) {
    const { success, fail, complete } = options;
    
    try {
        setTimeout(() => {
            const result = {
                errMsg: "updateApp:ok"
            };
            
            if (typeof success === 'function') {
                success(result);
            }

            if (typeof complete === 'function') {
                complete(result);
            }
        }, 100);
        
        return Promise.resolve({
            errMsg: "updateApp:ok"
        });
        
    } catch (error) {
        const errorResult = {
            errMsg: failMessage("updateApp", error)
        };
        
        if (typeof fail === 'function') {
            fail(errorResult);
        }
        
        if (typeof complete === 'function') {
            complete(errorResult);
        }
        
        return Promise.reject(errorResult);
    }
}

export { updateApp };