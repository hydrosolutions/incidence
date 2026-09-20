"""Synthetic public-interpreter coupling investigation, not production code."""
import copy
import incidence as i
import pytest

RID = bytes([22])*16

def partition(values):
    return {"rule_ir_version":"v1", "numerical_semantics_version":"v1", "partition":{"kind":"expression_partition", "branches":[{"branch":b,"expression":e} for b,e in values.items()]}}

def document(initial, incoming, outgoing, quanta, steps=1):
    substances=list(initial)
    forcings=[]
    rules=[]
    bindings=[]
    for s in substances:
        for node, branches in [("supply", {"pool": incoming[s]}), ("pool", outgoing[s])]:
            exprs={}
            for dest, amounts in branches.items():
                name=f"{node}-{s}-{dest}"
                forcings.append({"id":name,"horizon":{"first":0,"last":steps-1},"values":amounts})
                exprs[dest]=i.forcing(name)
                bindings.append({"compartment":node,"substance":s,"branch":dest,"destination":dest})
            rules.append(i.rule(node,s,i.literal(0),partition(exprs)))
    return i.model_document(finite_compartments=["supply","pool"],boundary_accounts=["left","right"], connections=[{"source":"supply","target":"pool"},{"source":"pool","target":"left"},{"source":"pool","target":"right"}],substances=substances,initial_stocks=[{"compartment":n,"amounts":[{"substance":s,"amount": initial[s] if n=="pool" else sum(incoming[s])} for s in substances]} for n in ["pool","supply"]],calendar={"origin_unix_seconds":0,"timestep_seconds":1},horizon={"first":0,"last":steps-1},projections={"specifications":[],"initial_states":[]},forcings=forcings,interpolation_tables=[],rules=rules,transfer_bindings=bindings,input_bindings=[],units=[{"substance":s,"unit":"unit","quantum":quanta[s]} for s in substances])

def counts(run,node,s,direction="incoming"):
    return list(run.transfer_count_series(node,s,direction=direction).values)

def zero_document():
    return document({"water":1,"mass":1},{"water":[0],"mass":[0]}, {s:{"left":[.5],"right":[.5]} for s in ["water","mass"]},{"water":1,"mass":.1})

def test_desired_zero_carrier_carries_zero_mass():
    run=i.compile_model(zero_document()).run(RID)
    actual={s:[counts(run,b,s)[0] for b in ["left","right"]] for s in ["water","mass"]}
    print("ACTUAL_ZERO",actual,flush=True)
    assert actual=={"water":[0,0],"mass":[0,0]}


def test_staged_complete_mixing_nonround_recurrence_multiple_constituents():
    water_doc=document({"water":2},{"water":[3,2,0]}, {"water":{"left":[1.5,2.4,1],"right":[2.5,1.6,0]}},{"water":1},3)
    water=i.compile_model(water_doc).run(RID)
    released={b:counts(water,b,"water") for b in ["left","right"]}
    assert released=={"left":[1,2,1],"right":[2,1,0]}
    water_pool=2
    initial={"salt":4,"tracer":11}
    inputs={"salt":[6,3,0],"tracer":[7,5,0]}
    pools=initial.copy()
    outputs={s:{b:[] for b in released} for s in initial}
    retained=[]
    for t in range(3):
        water_pool += [3,2,0][t]
        for s in initial:
            pools[s]+=inputs[s][t]
            allocations={b:pools[s]*released[b][t]//water_pool for b in released}
            for b,v in allocations.items(): outputs[s][b].append(v)
            pools[s]-=sum(allocations.values())
        water_pool-=sum(released[b][t] for b in released)
        retained.append((water_pool,pools.copy()))
    doc=document(initial,inputs,outputs,{s:1 for s in initial},3)
    model=i.compile_model(doc)
    mass=model.run(RID)
    for s in initial:
        for b in released: assert counts(mass,b,s)==outputs[s][b]
        assert sum(counts(mass,"pool",s,"outgoing"))+pools[s]==initial[s]+sum(inputs[s])
    assert outputs["salt"]=={"left":[2,3,3],"right":[4,1,0]}
    assert retained==[(2,{"salt":4,"tracer":8}),(1,{"salt":3,"tracer":4}),(0,{"salt":0,"tracer":0})]
    print("STAGED",released, outputs, retained,flush=True)
    assert mass.authoritative_log()==model.run(RID).authoritative_log()
    mass.replay_against(model.run(RID))
    changed=copy.deepcopy(doc); changed["units"][0]["quantum"]=0.5
    other=i.compile_model(changed)
    assert other.model_digest!=model.model_digest
    with pytest.raises(ValueError): mass.replay_against(other.run(RID))


def test_staged_float_reentry_does_not_preserve_exact_counts():
    from test_authoritative_count_series import merged_count_document, projection_spec, MERGED_COUNT, QUANTUM
    doc=merged_count_document()
    doc["finite_compartments"].append("sink")
    doc["boundary_accounts"]=["terminal"]
    doc["connections"].append({"source":"sink","target":"terminal"})
    doc["transfer_bindings"].append({"compartment":"sink","substance":"water","branch":"final","destination":"terminal"})
    doc["rules"].append(i.rule("sink","water",i.literal(MERGED_COUNT*QUANTUM),i.release_all("final")))
    scalar=i.compile_model(doc).run(RID)
    actual=counts(scalar,"terminal","water")[0]
    assert counts(scalar,"sink","water")[0]==MERGED_COUNT
    assert actual!=MERGED_COUNT
    print("FLOAT_REENTRY",MERGED_COUNT,actual,flush=True)
    doc["projections"]["specifications"].append(projection_spec("sink-incoming","sink"))
    doc["projections"]["initial_states"].append({"projection":"sink-incoming","values":[]})
    doc["rules"][-1]["expression"]=i.projection("sink-incoming",value_kind="extensive")
    exact=i.compile_model(doc).run(RID)
    assert counts(exact,"terminal","water")==[MERGED_COUNT]


def test_same_turn_outgoing_projection_cannot_observe_carrier_allocation():
    doc=zero_document()
    doc["initial_stocks"][0]["amounts"][0]["amount"]=3
    doc["forcings"][1]["values"]=[1.5]
    doc["forcings"][2]["values"]=[1.5]
    spec={"rule_ir_version":"v1","numerical_semantics_version":"v1","id":"carrier-out","value_kind":"extensive","spec":{"kind":"ordered_rolling_aggregate","source":{"kind":"authoritative_fact","selector":{"kind":"outgoing_transfer_amount","compartment":"pool","substance":"water"}},"window":1,"aggregate":"sum_oldest_to_newest"}}
    doc["projections"]={"specifications":[spec],"initial_states":[{"projection":"carrier-out","values":[]}]}
    doc["rules"][-1]["disposition"]=partition({"left":i.mul(i.literal(.1),i.projection("carrier-out",value_kind="extensive")),"right":i.literal(0)})
    run=i.compile_model(doc).run(RID)
    assert counts(run,"pool","water","outgoing")==[2]
    assert counts(run,"pool","mass","outgoing")==[0]
    print("SAME_TURN",counts(run,"pool","water","outgoing"),counts(run,"pool","mass","outgoing"),flush=True)


def coupled_zero_document():
    carrier_doc=document({"water":1},{"water":[0]}, {"water":{"left":[.5],"right":[.5]}},{"water":1})
    carrier=i.compile_model(carrier_doc).run(RID)
    mass_doc=document({"mass":1},{"mass":[0]}, {"mass":{b:[10*counts(carrier,b,"water")[0]//1*.1] for b in ["left","right"]}},{"mass":.1})
    # Experimental provenance binding using a public forcing identity, not a proposed production schema.
    identity="carrier-"+carrier.model_digest+"-complete-mixing-floor-v1"
    mass_doc["forcings"].append({"id":identity,"horizon":{"first":0,"last":0},"values":[0]})
    return carrier, mass_doc


def test_staged_zero_carrier_carries_zero_mass_and_binds_identity():
    carrier,doc=coupled_zero_document()
    model=i.compile_model(doc)
    mass=model.run(RID)
    actual={"water":[counts(carrier,b,"water")[0] for b in ["left","right"]],"mass":[counts(mass,b,"mass")[0] for b in ["left","right"]]}
    assert actual=={"water":[0,0],"mass":[0,0]}
    changed=copy.deepcopy(doc)
    changed["forcings"][-1]["id"]=changed["forcings"][-1]["id"].replace("complete-mixing","incoming-only")
    other=i.compile_model(changed).run(RID)
    assert other.model_digest!=mass.model_digest
    with pytest.raises(ValueError): mass.replay_against(other)
    print("STAGED_ZERO",actual,"IDENTITY",mass.model_digest,other.model_digest,flush=True)


def test_multisubstance_delay_existing_primitives():
    from test_authoritative_count_series import projection_spec
    doc=document({"water":0,"mass":0},{"water":[10,0],"mass":[5,0]}, {s:{"left":[0,0],"right":[0,0]} for s in ["water","mass"]},{"water":1,"mass":1},2)
    for s in ["water","mass"]:
        spec=projection_spec(s+"-lag","pool")
        spec["spec"]["source"]["selector"]["substance"]=s
        spec["spec"]={"kind":"bounded_lag","source":spec["spec"]["source"],"steps":1}
        doc["projections"]["specifications"].append(spec)
        doc["projections"]["initial_states"].append({"projection":s+"-lag","values":[{"kind":"extensive","value":0}]})
        for rule in doc["rules"]:
            if rule["compartment"]=="pool" and rule["substance"]==s:
                rule["disposition"]=partition({"left":i.projection(s+"-lag",value_kind="extensive"),"right":i.literal(0)})
    run=i.compile_model(doc).run(RID)
    assert counts(run,"left","water")==[0,10]
    assert counts(run,"left","mass")==[0,5]
    assert counts(run,"pool","mass")==[5,0]

def test_dry_inventory_existing_primitives():
    # t0 dry salt survives; clean t1 supply and declared complete remobilisation releases it.
    dry=document({"water":2,"mass":1},{"water":[0,4],"mass":[0,0]}, {"water":{"left":[2,0],"right":[0,4]},"mass":{"left":[0,0],"right":[0,1]}},{"water":1,"mass":1},2)
    dryrun=i.compile_model(dry).run(RID)
    assert counts(dryrun,"left","mass")==[0,0]
    assert counts(dryrun,"right","mass")==[0,1]
    assert counts(dryrun,"right","water")==[0,4]
    print("DRY", "dry retained 1 then rewet out 4/1",flush=True)


def test_single_substance_delay_existing_primitives():
    from test_authoritative_count_series import projection_spec
    doc=document({"water":0},{"water":[10,0]}, {"water":{"left":[0,0],"right":[0,0]}},{"water":1},2)
    spec=projection_spec("in-lag","pool")
    spec["spec"]={"kind":"bounded_lag","source":spec["spec"]["source"],"steps":1}
    doc["projections"]={"specifications":[spec],"initial_states":[{"projection":"in-lag","values":[{"kind":"extensive","value":0}]}]}
    doc["rules"][-1]["disposition"]=partition({"left":i.projection("in-lag",value_kind="extensive"),"right":i.literal(0)})
    run=i.compile_model(doc).run(RID)
    assert counts(run,"left","water")==[0,10]
    assert counts(run,"pool","water")==[10,0]
    print("ONE_INTERVAL_DELAY",counts(run,"left","water"),flush=True)


@pytest.mark.parametrize("initial_w,initial_m,in_w,in_m,left,right,expected", [
    (0,0,15,9,6,9,(36,54,0)),
    (20,4,10,4,30,0,(80,0,0)),
    (20,4,10,4,15,0,(40,0,40)),
    (0,0,15,9,10,5,(60,30,0)),
])
def test_staged_acceptance_pools(initial_w,initial_m,in_w,in_m,left,right,expected):
    water_doc=document({"water":initial_w},{"water":[in_w]}, {"water":{"left":[left],"right":[right]}},{"water":1})
    carrier=i.compile_model(water_doc).run(RID)
    pool_w=initial_w+in_w
    pool_m=round((initial_m+in_m)*10)
    target={b:pool_m*counts(carrier,b,"water")[0]//pool_w for b in ["left","right"]}
    mass_doc=document({"mass":initial_m},{"mass":[in_m]}, {"mass":{b:[v*.1] for b,v in target.items()}},{"mass":.1})
    mass=i.compile_model(mass_doc).run(RID)
    actual=tuple(counts(mass,b,"mass")[0] for b in ["left","right"])
    assert (*actual,pool_m-sum(actual))==expected
    print("ACCEPTANCE",initial_w,initial_m,in_w,in_m,left,right,expected,flush=True)
