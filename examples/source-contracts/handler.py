def set_status(request):
    if not request["allowed"]:
        return {"success": False}
    write = commit_status(request["project"], request["enabled"], request["epoch"])
    if write["rejected"]:
        return {"success": False}
    return {"success": True}
