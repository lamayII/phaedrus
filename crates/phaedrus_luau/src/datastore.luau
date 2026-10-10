-- DataStoreService emulator: the Roblox-facing objects.
--
-- Storage, validation and failure simulation live in the Rust backend
-- (datastore.rs). This layer builds DataStore, OrderedDataStore, pages,
-- DataStoreKeyInfo and the option objects, and does the yielding parts of
-- each request (queueing on the request budget and simulated latency).

local rawBackend, task, warn = ...

-- Backend errors reach Luau as wrapped host errors whose text carries a
-- "runtime error: " prefix and a Rust-side traceback. Rethrow them as plain
-- strings, like Roblox's own errors, so `pcall` handlers can match them.
local backend = {}
for name, fn in rawBackend do
	backend[name] = function(...)
		local result = table.pack(pcall(fn, ...))
		if not result[1] then
			local message = tostring(result[2])
			message = string.gsub(message, "^runtime error: ", "")
			message = string.match(message, "^(.-)\nstack traceback:") or message
			error(message, 0)
		end
		return table.unpack(result, 2, result.n)
	end
end

local NOT_ENABLED = "You must publish this place to the web to access DataStore."
local MAX_NAME = 50
local MAX_KEY = 50
local GLOBAL_STORE = "$GlobalDataStore"

local SUPER = {
	GlobalDataStore = "Instance",
	DataStore = "GlobalDataStore",
	OrderedDataStore = "GlobalDataStore",
	Pages = "Instance",
	DataStorePages = "Pages",
	DataStoreKeyPages = "Pages",
	DataStoreVersionPages = "Pages",
	DataStoreListingPages = "Pages",
	DataStoreKeyInfo = "Object",
	DataStoreKey = "Object",
	DataStoreObjectVersionInfo = "Object",
	DataStoreInfo = "Object",
	DataStoreOptions = "Instance",
	DataStoreGetOptions = "Instance",
	DataStoreSetOptions = "Instance",
	DataStoreIncrementOptions = "Instance",
	Instance = "Object",
}

local function isA(className, other)
	local c = className
	while c do
		if c == other then
			return true
		end
		c = SUPER[c]
	end
	return false
end

-- An Instance-like userdata. `fields` is read live, so methods can update
-- it; `writable` maps assignable field names to their Luau type.
local function object(className, name, fields, methods, writable)
	local function index(_, k)
		if k == "ClassName" then
			return className
		elseif k == "Name" then
			return name
		elseif k == "IsA" then
			return function(_, other)
				return isA(className, other)
			end
		elseif k == "GetFullName" then
			return function()
				return name
			end
		elseif k == "Destroy" then
			return function() end
		end
		local m = methods and methods[k]
		if m ~= nil then
			return m
		end
		local v = fields and fields[k]
		if v ~= nil then
			return v
		end
		error(`{k} is not a valid member of {className} "{name}"`, 0)
	end
	local function newindex(_, k, v)
		local want = writable and writable[k]
		if not want then
			if fields and fields[k] ~= nil or k == "ClassName" then
				error(`Unable to assign property {k}. Property is read only`, 0)
			end
			error(`{k} is not a valid member of {className} "{name}"`, 0)
		end
		if type(v) ~= want then
			error(`Unable to assign property {k}. {want} expected, got {typeof(v)}`, 0)
		end
		fields[k] = v
	end
	return backend.object(name, index, newindex)
end

local function isObject(v, className)
	return typeof(v) == "Instance" and v.ClassName == className
end

local function checkEnabled()
	if not backend.enabled() then
		error(NOT_ENABLED, 0)
	end
end

local function checkKey(key, argIndex)
	if type(key) == "number" then
		key = tostring(key)
	end
	if type(key) ~= "string" then
		error(`Argument {argIndex or 1} missing or nil`, 0)
	end
	if #key == 0 then
		error("101: Key name can't be empty.", 0)
	end
	if #key > MAX_KEY then
		error(`102: Key name exceeds the {MAX_KEY} character limit.`, 0)
	end
	return key
end

local function checkName(what, s, allowEmpty)
	if type(s) ~= "string" then
		error(`{what} must be a string`, 0)
	end
	if #s == 0 and not allowEmpty then
		error(`{what} can't be empty`, 0)
	end
	if #s > MAX_NAME then
		error(`{what} exceeds the {MAX_NAME} character limit`, 0)
	end
end

local function checkMetadataOptions(options, className, argIndex)
	if options == nil then
		return nil
	end
	if not isObject(options, className) then
		error(`Argument {argIndex} must be a {className}`, 0)
	end
	return options:GetMetadata()
end

-- One request: wait for budget, wait out the simulated latency, then maybe
-- fail. Always yields at least one frame, like a real request.
local function request(op, budgetType, store, scope, key, isWrite)
	checkEnabled()
	local queued = backend.acquire(budgetType, store, scope, key, isWrite)
	if queued then
		warn(
			`DataStore request was added to queue. If request queue fills, further requests will be dropped. Try sending fewer requests.Key = {key or ""}`
		)
		while queued do
			task.wait(queued)
			checkEnabled()
			queued = backend.acquire(budgetType, store, scope, key, isWrite)
		end
	end
	task.wait(backend.latency())
	checkEnabled()
	local failure = backend.check(op, store, key)
	if failure then
		error(failure, 0)
	end
end

-- Calls an UpdateAsync transform; it must not yield.
local function runTransform(transform, ...)
	local co = coroutine.create(transform)
	local result = table.pack(coroutine.resume(co, ...))
	if not result[1] then
		error(result[2], 0)
	end
	if coroutine.status(co) ~= "dead" then
		error("UpdateAsync transform function cannot yield", 0)
	end
	return result[2], result[3], result[4]
end

local function keyInfo(info)
	if info == nil then
		return nil
	end
	return object("DataStoreKeyInfo", "DataStoreKeyInfo", {
		CreatedTime = info.CreatedTime,
		UpdatedTime = info.UpdatedTime,
		Version = info.Version,
	}, {
		GetUserIds = function()
			return table.clone(info.UserIds)
		end,
		GetMetadata = function()
			return table.clone(info.Metadata)
		end,
	})
end

-- Pages over a snapshot taken when the listing was requested. Advancing
-- counts as a request of `op`.
local function pages(className, op, budgetType, store, items, pageSize, startIndex, wrap)
	local index = startIndex
	local fields = {}
	local function update()
		fields.IsFinished = index + pageSize > #items
		if className == "DataStoreKeyPages" or className == "DataStoreListingPages" then
			fields.Cursor = fields.IsFinished and "" or tostring(index + pageSize)
		end
	end
	update()
	return object(className, className, fields, {
		GetCurrentPage = function()
			local out = {}
			for i = index, math.min(index + pageSize - 1, #items) do
				table.insert(out, wrap(items[i]))
			end
			return out
		end,
		AdvanceToNextPageAsync = function()
			if fields.IsFinished then
				error("Cannot advance to the next page: no pages left", 0)
			end
			request(op, budgetType, store, "", nil, false)
			index += pageSize
			update()
		end,
	})
end

local function pageSizeArg(n, default, max, argIndex)
	if n == nil or n == 0 then
		return default
	end
	if type(n) ~= "number" or n < 1 or n ~= math.floor(n) or (max and n > max) then
		error(`Argument {argIndex}: page size must be an integer between 1 and {max or "infinity"}`, 0)
	end
	return n
end

local function cursorArg(cursor)
	if cursor == nil or cursor == "" then
		return 1
	end
	local n = tonumber(cursor)
	if not n or n < 1 then
		error("Invalid cursor", 0)
	end
	return n
end

-- --- DataStore ----------------------------------------------------------------

local stores = {}

local function newDataStore(name, scope, allScopes)
	local cacheKey = `D\0{name}\0{scope}\0{allScopes}`
	if stores[cacheKey] then
		return stores[cacheKey]
	end

	-- Resolves the scope and key a request addresses (AllScopes keys are
	-- written "scope/key").
	local function resolve(key, argIndex)
		key = checkKey(key, argIndex)
		if not allScopes then
			return scope, key
		end
		local s, k = string.match(key, "^([^/]*)/(.+)$")
		if not s then
			error(`Key "{key}" must be "scope/key" when DataStoreOptions.AllScopes is true`, 0)
		end
		return s, k
	end

	local methods = {}

	function methods.GetAsync(_, key, options)
		local s, k = resolve(key)
		if options ~= nil and not isObject(options, "DataStoreGetOptions") then
			error("Argument 2 must be a DataStoreGetOptions", 0)
		end
		request("GetAsync", "GetAsync", name, s, k, false)
		local value, info = backend.get(name, s, k)
		return value, keyInfo(info)
	end

	function methods.SetAsync(_, key, value, userIds, options)
		local s, k = resolve(key)
		local encoded = backend.encode(value)
		local metadata = checkMetadataOptions(options, "DataStoreSetOptions", 4)
		request("SetAsync", "SetIncrementAsync", name, s, k, true)
		return backend.set(name, s, k, encoded, userIds, metadata)
	end

	function methods.UpdateAsync(_, key, transform)
		local s, k = resolve(key)
		if type(transform) ~= "function" then
			error("Argument 2 missing or nil", 0)
		end
		request("UpdateAsync", "UpdateAsync", name, s, k, true)
		local current, info = backend.get(name, s, k)
		local newValue, userIds, metadata = runTransform(transform, current, keyInfo(info))
		if newValue == nil then
			-- Returning nil cancels the update.
			return nil
		end
		backend.set(name, s, k, backend.encode(newValue), userIds, metadata)
		local value, newInfo = backend.get(name, s, k)
		return value, keyInfo(newInfo)
	end

	function methods.IncrementAsync(_, key, delta, userIds, options)
		local s, k = resolve(key)
		delta = if delta == nil then 1 else tonumber(delta)
		if delta == nil then
			error("Argument 2 must be a number", 0)
		end
		local metadata = checkMetadataOptions(options, "DataStoreIncrementOptions", 4)
		request("IncrementAsync", "SetIncrementAsync", name, s, k, true)
		return (backend.increment(name, s, k, delta, userIds, metadata))
	end

	function methods.RemoveAsync(_, key)
		local s, k = resolve(key)
		request("RemoveAsync", "SetIncrementAsync", name, s, k, true)
		local value, info = backend.remove(name, s, k)
		return value, keyInfo(info)
	end

	function methods.ListKeysAsync(_, prefix, pageSize, cursor, excludeDeleted)
		prefix = prefix or ""
		if type(prefix) ~= "string" then
			error("Argument 1 must be a string", 0)
		end
		pageSize = pageSizeArg(pageSize, 50, nil, 2)
		local start = cursorArg(cursor)
		request("ListKeysAsync", "ListAsync", name, scope, nil, false)
		local keys = backend.list_keys(name, scope, allScopes, prefix, excludeDeleted == true)
		return pages("DataStoreKeyPages", "ListKeysAsync", "ListAsync", name, keys, pageSize, start, function(k)
			return object("DataStoreKey", k, { KeyName = k })
		end)
	end

	function methods.ListVersionsAsync(_, key, sortDirection, minDate, maxDate, pageSize)
		local s, k = resolve(key)
		local ascending = true
		if sortDirection ~= nil then
			if typeof(sortDirection) ~= "EnumItem" or not sortDirection:IsA("SortDirection") then
				error("Argument 2 must be an Enum.SortDirection", 0)
			end
			ascending = sortDirection.Name ~= "Descending"
		end
		pageSize = pageSizeArg(pageSize, 1, nil, 5)
		request("ListVersionsAsync", "ListAsync", name, s, k, false)
		local versions = backend.list_versions(name, s, k, ascending, tonumber(minDate) or 0, tonumber(maxDate) or 0)
		return pages("DataStoreVersionPages", "ListVersionsAsync", "ListAsync", name, versions, pageSize, 1, function(v)
			return object("DataStoreObjectVersionInfo", "DataStoreObjectVersionInfo", {
				CreatedTime = v.CreatedTime,
				IsDeleted = v.IsDeleted,
				Version = v.Version,
			})
		end)
	end

	function methods.GetVersionAsync(_, key, version)
		local s, k = resolve(key)
		if type(version) ~= "string" then
			error("Argument 2 missing or nil", 0)
		end
		request("GetVersionAsync", "GetVersionAsync", name, s, k, false)
		local value, info = backend.get_version(name, s, k, version)
		return value, keyInfo(info)
	end

	function methods.GetVersionAtTimeAsync(_, key, timestamp)
		local s, k = resolve(key)
		if type(timestamp) ~= "number" then
			error("Argument 2 must be a Unix timestamp in milliseconds", 0)
		end
		request("GetVersionAsync", "GetVersionAsync", name, s, k, false)
		local value, info = backend.get_version_at(name, s, k, timestamp)
		return value, keyInfo(info)
	end

	function methods.RemoveVersionAsync(_, key, version)
		local s, k = resolve(key)
		if type(version) ~= "string" then
			error("Argument 2 missing or nil", 0)
		end
		request("RemoveVersionAsync", "RemoveVersionAsync", name, s, k, true)
		backend.remove_version(name, s, k, version)
	end

	function methods.OnUpdate()
		error("OnUpdate is deprecated and not supported by the DataStoreService emulator", 2)
	end

	local display = if name == GLOBAL_STORE then "GlobalDataStore" else name
	local store = object(if name == GLOBAL_STORE then "GlobalDataStore" else "DataStore", display, {}, methods)
	stores[cacheKey] = store
	return store
end

-- --- OrderedDataStore ---------------------------------------------------------

local function checkInteger(v, argIndex)
	if type(v) ~= "number" or v ~= math.floor(v) or v ~= v then
		error(`Argument {argIndex}: OrderedDataStore values must be integers, got {typeof(v)}`, 0)
	end
	return v
end

local function newOrderedDataStore(name, scope)
	local cacheKey = `O\0{name}\0{scope}`
	if stores[cacheKey] then
		return stores[cacheKey]
	end
	local methods = {}

	function methods.GetAsync(_, key)
		key = checkKey(key)
		request("GetAsync", "GetAsync", name, scope, key, false)
		return backend.ordered_get(name, scope, key)
	end

	function methods.SetAsync(_, key, value)
		key = checkKey(key)
		checkInteger(value, 2)
		request("SetAsync", "SetIncrementSortedAsync", name, scope, key, true)
		backend.ordered_set(name, scope, key, value)
		return nil
	end

	function methods.UpdateAsync(_, key, transform)
		key = checkKey(key)
		if type(transform) ~= "function" then
			error("Argument 2 missing or nil", 0)
		end
		request("UpdateAsync", "SetIncrementSortedAsync", name, scope, key, true)
		local newValue = runTransform(transform, backend.ordered_get(name, scope, key))
		if newValue == nil then
			return nil
		end
		checkInteger(newValue, 2)
		backend.ordered_set(name, scope, key, newValue)
		return newValue
	end

	function methods.IncrementAsync(_, key, delta)
		key = checkKey(key)
		delta = if delta == nil then 1 else checkInteger(tonumber(delta), 2)
		request("IncrementAsync", "SetIncrementSortedAsync", name, scope, key, true)
		local value = (backend.ordered_get(name, scope, key) or 0) + delta
		backend.ordered_set(name, scope, key, value)
		return value
	end

	function methods.RemoveAsync(_, key)
		key = checkKey(key)
		request("RemoveAsync", "SetIncrementSortedAsync", name, scope, key, true)
		return backend.ordered_remove(name, scope, key)
	end

	function methods.GetSortedAsync(_, ascending, pageSize, minValue, maxValue)
		if type(ascending) ~= "boolean" then
			error("Argument 1 missing or nil", 0)
		end
		if pageSize == nil then
			error("Argument 2 missing or nil", 0)
		end
		pageSize = pageSizeArg(pageSize, 50, 100, 2)
		if minValue ~= nil then
			checkInteger(minValue, 3)
		end
		if maxValue ~= nil then
			checkInteger(maxValue, 4)
		end
		request("GetSortedAsync", "GetSortedAsync", name, scope, nil, false)
		local items = backend.sorted(name, scope, ascending, minValue, maxValue)
		return pages("DataStorePages", "GetSortedAsync", "GetSortedAsync", name, items, pageSize, 1, function(item)
			return { key = item.key, value = item.value }
		end)
	end

	local store = object("OrderedDataStore", name, {}, methods)
	stores[cacheKey] = store
	return store
end

-- --- DataStoreService -----------------------------------------------------------

local warnedDisabled = false
local function checkServiceEnabled()
	if not backend.enabled() then
		if not warnedDisabled then
			warnedDisabled = true
			warn("DataStoreService is offline. Enable the DataStore emulator (Test > Emulators) to use data stores in Phaedrus.")
		end
		error(NOT_ENABLED, 0)
	end
end

local service = {}

function service.GetDataStore(_, name, scope, options)
	checkServiceEnabled()
	checkName("DataStore name", name)
	local allScopes = false
	if options ~= nil then
		if not isObject(options, "DataStoreOptions") then
			error("Argument 3 must be a DataStoreOptions", 0)
		end
		allScopes = options.AllScopes
	end
	if allScopes then
		if scope ~= nil and scope ~= "" then
			error("DataStore scope must be empty when DataStoreOptions.AllScopes is true", 0)
		end
		scope = ""
	else
		scope = scope or "global"
		checkName("DataStore scope", scope)
	end
	return newDataStore(name, scope, allScopes)
end

function service.GetGlobalDataStore()
	checkServiceEnabled()
	return newDataStore(GLOBAL_STORE, "global", false)
end

function service.GetOrderedDataStore(_, name, scope)
	checkServiceEnabled()
	checkName("OrderedDataStore name", name)
	scope = scope or "global"
	checkName("OrderedDataStore scope", scope)
	return newOrderedDataStore(name, scope)
end

function service.GetRequestBudgetForRequestType(_, requestType)
	checkServiceEnabled()
	if typeof(requestType) ~= "EnumItem" or not requestType:IsA("DataStoreRequestType") then
		error("Argument 1 must be an Enum.DataStoreRequestType", 0)
	end
	return backend.budget(requestType.Name)
end

function service.ListDataStoresAsync(_, prefix, pageSize, cursor)
	checkServiceEnabled()
	prefix = prefix or ""
	if type(prefix) ~= "string" then
		error("Argument 1 must be a string", 0)
	end
	pageSize = pageSizeArg(pageSize, 32, nil, 2)
	local start = cursorArg(cursor)
	request("ListDataStoresAsync", "ListAsync", "", "", nil, false)
	local list = backend.list_stores(prefix)
	return pages("DataStoreListingPages", "ListDataStoresAsync", "ListAsync", "", list, pageSize, start, function(info)
		return object("DataStoreInfo", info.DataStoreName, {
			CreatedTime = info.CreatedTime,
			DataStoreName = info.DataStoreName,
			UpdatedTime = info.UpdatedTime,
		})
	end)
end

-- --- options (Instance.new) -----------------------------------------------------

local function metadataMethods()
	local metadata = {}
	return {
		SetMetadata = function(_, t)
			if type(t) ~= "table" then
				error("Argument 1 must be a table", 0)
			end
			metadata = table.clone(t)
		end,
		GetMetadata = function()
			return table.clone(metadata)
		end,
	}
end

local function newOption(className)
	if className == "DataStoreOptions" then
		return object(className, className, { AllScopes = false }, {
			SetExperimentalFeatures = function() end,
		}, { AllScopes = "boolean" })
	elseif className == "DataStoreGetOptions" then
		return object(className, className, { UseCache = true }, {}, { UseCache = "boolean" })
	elseif className == "DataStoreSetOptions" or className == "DataStoreIncrementOptions" then
		return object(className, className, {}, metadataMethods())
	end
	error(`Unable to create an Instance of type "{className}"`, 2)
end

return service, newOption
