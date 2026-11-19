package example

type Request struct {
	allowed bool
	enabled bool
	project string
	epoch   string
}
type Response struct{ success bool }
type WriteResult struct{ rejected bool }

// The enclosing application supplies this persistence dependency.
var commitStatus func(project string, enabled bool, epoch string) WriteResult

func setStatus(request Request) Response {
	if !request.allowed {
		return Response{success: false}
	}
	write := commitStatus(request.project, request.enabled, request.epoch)
	if write.rejected {
		return Response{success: false}
	}
	return Response{success: true}
}
