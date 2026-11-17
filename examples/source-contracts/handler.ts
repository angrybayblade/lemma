type Request = { allowed: boolean; enabled: boolean; project: string; epoch: string };
type Response = { success: boolean };
declare function commitStatus(project: string, enabled: boolean, epoch: string): { rejected: boolean };

export function setStatus(request: Request): Response {
    if (!request.allowed) {
        return { success: false };
    }
    const write = commitStatus(request.project, request.enabled, request.epoch);
    if (write.rejected) {
        return { success: false };
    }
    return { success: true };
}
